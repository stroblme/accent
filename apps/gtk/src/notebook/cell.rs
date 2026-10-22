//! One cell's widgets: its source, the `[n]` its run was counted as, its outputs under it, and the
//! strip below it where a cell can be added after it. The source is a plain label of the same
//! text until the cell nears the view and the tab builds its editor (`NotebookTab::editor`). The
//! buttons and the strip's own buttons are the notebook's, one of each, moved to the cell they
//! serve ([`Tools`], [`inserter`]): a long notebook would otherwise build them per cell up front.

use std::cell::{OnceCell, RefCell};

use accent_core::notebook::{Cell, CellId, Format, Kind, Output};
use adw::prelude::*;
use gtk::gio;

use super::output::Outputs;
use crate::editor::Flavour;

pub struct CellView {
    pub id: CellId,
    pub kind: Kind,
    /// The cell and the strip under it, the unit the notebook's column holds.
    pub root: gtk::Box,
    /// The cell itself, outlined in the accent while selected.
    pub card: gtk::Box,
    /// Where the buttons float, over the cell's top right corner.
    pub over: gtk::Overlay,
    /// The strip under the cell, where the add buttons show while the pointer is over it.
    pub gap: gtk::Overlay,
    /// `[n]`, the count of the run that left the outputs; `[*]` while a run waits or runs.
    count: gtk::Label,
    /// Execution state shown beside Run Cell.
    pub execution: std::cell::Cell<Execution>,
    /// Last settled result while another request may still show Running.
    pub last_result: std::cell::Cell<Execution>,
    body: gtk::Box,
    /// The source as plain text in the editor's font, holding the editor's place until it is built.
    placeholder: gtk::Label,
    editor: OnceCell<Editor>,
    pub outputs: Outputs,
    /// What `input()` asks under the outputs, while it asks.
    asking: RefCell<Option<gtk::Box>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Execution {
    Unrun,
    Running,
    Succeeded,
    Failed,
    StaleSucceeded,
    StaleFailed,
}

impl Execution {
    pub(super) fn stale(self) -> Execution {
        match self {
            Execution::Succeeded | Execution::StaleSucceeded => Execution::StaleSucceeded,
            Execution::Failed | Execution::StaleFailed => Execution::StaleFailed,
            Execution::Unrun | Execution::Running => Execution::Unrun,
        }
    }

    fn for_cell(cell: &Cell) -> Execution {
        if cell
            .outputs()
            .iter()
            .any(|o| matches!(o, Output::Error { .. }))
        {
            Execution::Failed
        } else if cell.count().is_some() {
            Execution::Succeeded
        } else {
            Execution::Unrun
        }
    }

    fn label(self) -> &'static str {
        match self {
            Execution::Unrun => "Not run",
            Execution::Running => "Running",
            Execution::Succeeded => "Succeeded",
            Execution::Failed => "Failed",
            Execution::StaleSucceeded => "Previous run succeeded; result may be stale",
            Execution::StaleFailed => "Previous run failed; result may be stale",
        }
    }

    fn class(self) -> &'static str {
        match self {
            Execution::Unrun => "unrun",
            Execution::Running => "running",
            Execution::Succeeded => "succeeded",
            Execution::Failed => "failed",
            Execution::StaleSucceeded => "stale-succeeded",
            Execution::StaleFailed => "stale-failed",
        }
    }
}

/// A cell's editor, built once the cell nears the view.
#[derive(Clone)]
pub struct Editor {
    pub view: sourceview5::View,
    pub buffer: sourceview5::Buffer,
}

impl Editor {
    pub fn text(&self) -> String {
        let (start, end) = self.buffer.bounds();
        self.buffer.text(&start, &end, true).to_string()
    }
}

impl CellView {
    /// `cell`'s widgets, its source shown by a placeholder until [`build_editor`](Self::build_editor).
    pub fn new(cell: &Cell) -> CellView {
        let count = gtk::Label::builder()
            .label(count_label(cell))
            .width_chars(6)
            .xalign(1.0)
            .valign(gtk::Align::Start)
            .margin_top(8)
            .build();
        count.add_css_class("notebook-count");

        // A line for each line of the source, in the editor's font and margins, so the cell is
        // about as tall before its editor is built as after. Not wrapped: a wrapping label is
        // laid out several times over, and three hundred of them took longer than the editors.
        let placeholder = gtk::Label::builder()
            .label(cell.source())
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .xalign(0.0)
            .yalign(0.0)
            .margin_start(8)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        placeholder.add_css_class(match flavour(cell.kind()) {
            Flavour::Note => "accent-doc",
            _ => "accent-code",
        });

        let outputs = match cell.kind() {
            Kind::Code => Outputs::new(cell.outputs()),
            _ => Outputs::attachments(cell.attachments()),
        };
        let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
        body.set_hexpand(true);
        body.append(&placeholder);
        body.append(&outputs.widget);

        let over = gtk::Overlay::builder().child(&body).hexpand(true).build();
        let card = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        card.add_css_class("notebook-cell");
        card.append(&count);
        card.append(&over);

        let room = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let gap = gtk::Overlay::builder().child(&room).build();
        gap.add_css_class("notebook-gap");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&card);
        root.append(&gap);
        CellView {
            id: cell.id(),
            kind: cell.kind(),
            root,
            card,
            over,
            gap,
            count,
            execution: std::cell::Cell::new(Execution::for_cell(cell)),
            last_result: std::cell::Cell::new(Execution::for_cell(cell)),
            body,
            placeholder,
            editor: OnceCell::new(),
            outputs,
            asking: RefCell::default(),
        }
    }

    pub fn set_count(&self, text: &str) {
        if self.count.label() != text {
            self.count.set_label(text);
        }
    }

    /// Ask for a line under the outputs, as `input()` does: `prompt` before an entry, its
    /// characters hidden for a `password`, and Return handing what was typed to `answer`, the
    /// entry then kept, greyed, until the run ends ([`end_ask`](Self::end_ask)).
    pub fn ask(
        &self,
        prompt: &str,
        password: bool,
        answer: impl Fn(String) + 'static,
    ) -> gtk::Widget {
        self.end_ask();
        let entry: gtk::Widget = match password {
            true => {
                let entry = gtk::PasswordEntry::builder()
                    .show_peek_icon(true)
                    .hexpand(true)
                    .build();
                entry.connect_activate(move |entry| {
                    entry.set_sensitive(false);
                    answer(entry.text().to_string());
                });
                entry.upcast()
            }
            false => {
                let entry = gtk::Entry::builder().hexpand(true).build();
                entry.connect_activate(move |entry| {
                    entry.set_sensitive(false);
                    answer(entry.text().to_string());
                });
                entry.upcast()
            }
        };
        let label = gtk::Label::new(Some(prompt));
        label.add_css_class("notebook-output");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.append(&label);
        row.append(&entry);
        self.body.append(&row);
        self.asking.replace(Some(row));
        entry
    }

    /// `input()` is answered, or its run over: the entry goes.
    pub fn end_ask(&self) {
        if let Some(row) = self.asking.take() {
            self.body.remove(&row);
        }
    }

    #[cfg(feature = "bench")]
    pub fn count_text(&self) -> String {
        self.count.label().to_string()
    }

    /// What `input()` asks: its prompt, whether the entry hides what is typed, and the entry.
    #[cfg(feature = "bench")]
    pub fn asking(&self) -> Option<(String, bool, gtk::Widget)> {
        let row = self.asking.borrow().clone()?;
        let prompt = row.first_child().and_downcast::<gtk::Label>()?;
        let entry = prompt.next_sibling()?;
        let password = entry.is::<gtk::PasswordEntry>();
        Some((prompt.label().to_string(), password, entry))
    }

    /// The editor, once built.
    pub fn editor(&self) -> Option<&Editor> {
        self.editor.get()
    }

    /// Build the editor over `source` in the placeholder's place, a code cell coloured as
    /// `language`, read only while `presenting`.
    pub fn build_editor(
        &self,
        source: &str,
        language: Option<&sourceview5::Language>,
        presenting: bool,
    ) -> Editor {
        let flavour = flavour(self.kind);
        let language = match self.kind {
            Kind::Code => language.cloned(),
            _ => None,
        };
        let (view, buffer) = crate::editor::build(flavour, language, source);
        // The notebook's one history records what is typed (`history.rs`).
        buffer.set_enable_undo(false);
        view.set_left_margin(8);
        view.set_right_margin(8);
        view.set_top_margin(6);
        view.set_bottom_margin(6);
        view.set_hexpand(true);
        view.set_editable(!presenting);
        if flavour == Flavour::Note {
            crate::highlight::apply(&buffer);
            // The tags' colours are read off the view's own, which it has once it is realised.
            view.connect_realize(|view| {
                if let Some(buffer) = view.buffer().downcast_ref::<sourceview5::Buffer>() {
                    crate::highlight::restyle(buffer, view);
                }
            });
        } else {
            view.add_css_class("notebook-source");
        }
        self.body.prepend(&view);
        self.body.remove(&self.placeholder);
        let editor = Editor { view, buffer };
        let _ = self.editor.set(editor.clone());
        editor
    }

    /// The cell's text views: its editor, once built, and its outputs drawn as text.
    pub fn text_views(&self) -> Vec<gtk::TextView> {
        let mut views = self.outputs.text_views();
        if let Some(editor) = self.editor() {
            views.insert(0, editor.view.clone().upcast());
        }
        views
    }

    /// Ask again for the height of each text view whose request no longer matches what it was
    /// given: a text view re-wraps to a new width inside its own allocation, and GTK drops the
    /// resize it asks for there (ISSUES.md), so it would keep its old height until the next scroll.
    pub fn refit(&self) {
        for view in self.text_views() {
            let width = view.width();
            if width > 0 && view.measure(gtk::Orientation::Vertical, width).1 != view.height() {
                view.queue_resize();
            }
        }
    }

    /// The editor's text, once it is built; until then the model's is the cell's.
    pub fn text(&self) -> Option<String> {
        self.editor().map(Editor::text)
    }

    pub fn shows_source(&self, source: &str) -> bool {
        self.editor().map_or_else(
            || self.placeholder.label() == source,
            |editor| editor.text() == source,
        )
    }

    /// The model's text for a cell with no editor yet.
    pub fn set_source(&self, source: &str) {
        if self.editor().is_none() && self.placeholder.label() != source {
            self.placeholder.set_label(source);
        }
    }

    pub fn set_selected(&self, on: bool) {
        match on {
            true => self.card.add_css_class("selected"),
            false => self.card.remove_css_class("selected"),
        }
    }

    /// Read only: a presented notebook.
    pub fn set_presenting(&self, on: bool) {
        if let Some(editor) = self.editor() {
            editor.view.set_editable(!on);
        }
    }
}

/// A code cell's `[n]`, `[ ]` for one never run; nothing for another kind.
pub fn count_label(cell: &Cell) -> String {
    match (cell.kind(), cell.count()) {
        (Kind::Code, Some(n)) => format!("[{n}]"),
        (Kind::Code, None) => "[ ]".to_string(),
        _ => String::new(),
    }
}

fn flavour(kind: Kind) -> Flavour {
    match kind {
        Kind::Markdown => Flavour::Note,
        Kind::Code | Kind::Raw => Flavour::Code,
    }
}

/// The cell buttons, one bar the notebook floats over the cell it serves: Run Cell over a code
/// cell of a notebook that runs, Move Cell Up, Move Cell Down, Delete Cell and a More menu with
/// the rest.
pub struct Tools {
    pub bar: gtk::Box,
    pub run: gtk::Button,
    status: gtk::Box,
    /// Whether the notebook's code cells run: an `.ipynb`'s, not marimo's.
    runs: bool,
    pub up: gtk::Button,
    pub down: gtk::Button,
    pub delete: gtk::Button,
    pub more: gtk::MenuButton,
    /// The cell it is over.
    pub cell: std::cell::Cell<Option<CellId>>,
}

impl Tools {
    pub fn new(format: Format) -> Tools {
        let button = |icon: &str, action: &str| {
            let b = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(crate::actions::label_of(action))
                .build();
            b.add_css_class("flat");
            b
        };
        let run = button("media-playback-start-symbolic", "win.notebook-run-cell");
        let status = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        status.add_css_class("notebook-execution");
        status.set_valign(gtk::Align::Center);
        let up = button("go-up-symbolic", "win.notebook-move-up");
        let down = button("go-down-symbolic", "win.notebook-move-down");
        let delete = button("user-trash-symbolic", "win.notebook-delete-cell");
        let more = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("More")
            .menu_model(&cell_menu())
            .build();
        more.add_css_class("flat");
        // Over the editor's top right corner, as VS Code floats a cell's toolbar, so a cell is
        // as tall as its text and its outputs.
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bar.add_css_class("notebook-tools");
        bar.set_halign(gtk::Align::End);
        bar.set_valign(gtk::Align::Start);
        bar.append(&status);
        for b in [&run, &up, &down, &delete] {
            bar.append(b);
        }
        bar.append(&more);
        Tools {
            bar,
            run,
            status,
            runs: format == Format::Ipynb,
            up,
            down,
            delete,
            more,
            cell: std::cell::Cell::new(None),
        }
    }

    /// Float over `view`, or nowhere.
    pub fn place(&self, view: Option<&CellView>) {
        let code = view.is_some_and(|v| v.kind == Kind::Code);
        self.run.set_visible(self.runs && code);
        self.status.set_visible(self.runs && code);
        if let Some(view) = view {
            self.show_execution(view.execution.get());
        }
        let target = view.map(|v| v.over.clone().upcast::<gtk::Widget>());
        if self.bar.parent() == target {
            return;
        }
        if let Some(over) = self.bar.parent().and_downcast::<gtk::Overlay>() {
            over.remove_overlay(&self.bar);
        }
        self.cell.set(view.map(|v| v.id));
        if let Some(view) = view {
            view.over.add_overlay(&self.bar);
        }
    }

    pub fn show_execution(&self, state: Execution) {
        for class in [
            "unrun",
            "running",
            "succeeded",
            "failed",
            "stale-succeeded",
            "stale-failed",
        ] {
            self.status.remove_css_class(class);
        }
        self.status.add_css_class(state.class());
        self.status.set_tooltip_text(Some(state.label()));
        self.run
            .update_property(&[gtk::accessible::Property::Description(state.label())]);
    }
}

/// The strip that adds a code or a markdown cell where it is, shown while the pointer is over it.
pub fn inserter() -> (gtk::Box, gtk::Button, gtk::Button) {
    let add = |label: &str| {
        let b = gtk::Button::builder().label(label).build();
        b.add_css_class("flat");
        b.add_css_class("caption");
        b
    };
    let (code, markdown) = (add("+ Code"), add("+ Markdown"));
    let strip = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    strip.set_halign(gtk::Align::Center);
    strip.add_css_class("notebook-inserter");
    strip.append(&code);
    strip.append(&markdown);
    (strip, code, markdown)
}

/// The cell's More menu: what the palette also does to the selected cell.
fn cell_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let section = |actions: &[&str]| {
        let s = gio::Menu::new();
        for action in actions {
            s.append(Some(crate::actions::label_of(action)), Some(action));
        }
        menu.append_section(None, &s);
    };
    section(&["win.notebook-run-cell", "win.notebook-clear-outputs"]);
    section(&["win.notebook-insert-above", "win.notebook-insert-below"]);
    section(&[
        "win.notebook-to-code",
        "win.notebook-to-markdown",
        "win.notebook-to-raw",
    ]);
    section(&["win.notebook-split", "win.notebook-join"]);
    menu
}

/// The stylesheet every notebook shares, installed with the first one. A cell's strip is as tall
/// as the add buttons, which are put over it while the pointer is there: shown at once, since a
/// widget put under a resting pointer is not hovered until the pointer moves.
pub fn install_css() {
    thread_local! {
        static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        ".notebook-cell { border: 1px solid transparent; border-radius: 8px; padding: 4px; } \
         .notebook-cell.selected { border-color: var(--accent-bg-color); } \
         .notebook-tools { margin: 2px; border-radius: 6px; \
           background-color: var(--view-bg-color); \
           box-shadow: 0 0 0 1px alpha(currentColor, 0.12); } \
         .notebook-tools button { min-height: 24px; min-width: 24px; padding: 0; } \
         .notebook-execution { min-width: 8px; min-height: 8px; margin: 0 4px 0 8px; \
           border: 1px solid transparent; border-radius: 50%; } \
         .notebook-execution.unrun { background-color: alpha(var(--window-fg-color), 0.3); } \
         .notebook-execution.running { background-color: var(--warning-color); } \
         .notebook-execution.succeeded { background-color: var(--success-color); } \
         .notebook-execution.failed { background-color: var(--error-color); } \
         .notebook-execution.stale-succeeded { border: 1px solid var(--success-color); } \
         .notebook-execution.stale-failed { border: 1px solid var(--error-color); } \
         .notebook-inserter { opacity: 0; } \
         .notebook-inserter button { min-height: 20px; padding: 0 10px; } \
         .notebook-gap { min-height: 20px; } \
         .notebook-inserter:hover, .notebook-inserter:focus-within, \
         .notebook-gap .notebook-inserter { opacity: 1; } \
         .notebook-count { opacity: var(--dim-opacity); font-family: monospace; } \
         textview.notebook-source { border-radius: 6px; \
           box-shadow: inset 0 0 0 1px alpha(currentColor, 0.12); } \
         textview.notebook-output, textview.notebook-output > text { background: none; } \
         textview.notebook-stderr > text { background-color: alpha(var(--error-color), 0.08); }",
    );
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use accent_core::notebook::{EMPTY_IPYNB, Edit, Run, read};

    #[test]
    fn stored_code_cells_show_their_last_known_result() {
        let mut notebook = read("empty.ipynb", EMPTY_IPYNB).unwrap();
        let id = notebook.cells()[0].id();
        assert_eq!(Execution::for_cell(&notebook.cells()[0]), Execution::Unrun);
        notebook.apply(Edit::Outputs {
            id,
            run: Some(Run::new(Some(1), Vec::new())),
        });
        assert_eq!(
            Execution::for_cell(&notebook.cells()[0]),
            Execution::Succeeded
        );
        let error = serde_json::json!({
            "output_type": "error", "ename": "ValueError", "evalue": "bad", "traceback": []
        });
        notebook.apply(Edit::Outputs {
            id,
            run: Some(Run::new(Some(2), vec![error])),
        });
        assert_eq!(Execution::for_cell(&notebook.cells()[0]), Execution::Failed);
    }
}
