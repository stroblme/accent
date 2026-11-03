//! A notebook's cells on the language layer. Its code cells are one document there, their program
//! (`Notebook::program`), opened as a code tab's file is, and its markdown cells another, one note
//! (`Notebook::prose`), opened as a note is; each cell's editor is a [`lang::Editor`] over that
//! cell's lines of its document, so hover, signature help, completion and the code tab's keys work
//! in a code cell as in a code tab, and hover, hints, completion, ghost text and a note's keys in a
//! markdown cell as in a note (DESIGN.md, Architecture › Notebooks).

use std::cell::{Cell, Ref, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use accent_api::{Diagnostic, Pos, Vault};
use accent_core::notebook::{CellId, Format, Kind, Program};
use gtk::glib;
use gtk::prelude::*;
use sourceview5::prelude::*;

use super::cell::{CellView, Editor};
use super::{NotebookTab, TabHook};
use crate::completion::{EditorSource, Reply, Session};
use crate::editor::Flavour;
use crate::{diagnostics, ghost, lang, signature};

/// A notebook's documents on the language layer, and its cells' editors there.
#[derive(Default)]
pub(super) struct State {
    /// The code cells' program, open as a code tab's file is.
    code: Layer,
    /// The markdown cells as one note, open as a note is.
    prose: Layer,
    editors: RefCell<HashMap<CellId, Rc<CellEditor>>>,
    /// The status count's press: every cell's diagnostics kept out of its text.
    hidden: Cell<bool>,
    /// The flush an edit schedules, replaced by the next edit's.
    refresh: RefCell<Option<glib::JoinHandle<()>>>,
    on_follow: TabHook,
    on_starting: TabHook,
    /// The Ghost Text preference, which the markdown cells follow as a note does.
    ghost: Cell<bool>,
}

impl State {
    /// The document cells of `kind` are in.
    fn layer(&self, kind: Kind) -> &Layer {
        match kind {
            Kind::Markdown => &self.prose,
            _ => &self.code,
        }
    }
}

/// One of a notebook's documents on the language layer: its cells of one kind as one text.
#[derive(Default)]
struct Layer {
    doc: Rc<lang::Document>,
    /// The text last handed to the server, whose lines its answers are in.
    sent: RefCell<Sent>,
    /// What the server said about cells with no editor yet, in their own lines.
    waiting: RefCell<HashMap<CellId, Vec<Diagnostic>>>,
}

/// The program last handed to the server, and whether it is known to have got there: a flush
/// dropped part way (by the next edit's, or a completion typed past) may have been cancelled
/// before it was sent, and a program taken as sent then would never be sent again.
#[derive(Default)]
struct Sent {
    program: Program,
    arrived: bool,
}

impl Sent {
    /// The text to send for `now`, which answers are read against from here on; `None` while the
    /// server holds it.
    fn next(&mut self, now: Program) -> Option<String> {
        if self.arrived && now == self.program {
            return None;
        }
        let text = now.text.clone();
        self.program = now;
        self.arrived = false;
        Some(text)
    }

    /// `text` reached the server.
    fn arrived(&mut self, text: &str) {
        if self.program.text == text {
            self.arrived = true;
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(handle) = self.refresh.take() {
            handle.abort();
        }
        for cell in self.editors.take().into_values() {
            cell.detach();
        }
        self.code.doc.close();
        self.prose.doc.close();
    }
}

/// A cell's editor as the language features see it: its view, over its cell's lines of the
/// notebook's document of its kind.
struct CellEditor {
    id: CellId,
    /// Code or markdown, which says the document its lines are in.
    kind: Kind,
    tab: Weak<NotebookTab>,
    doc: Rc<lang::Document>,
    view: sourceview5::View,
    buffer: sourceview5::Buffer,
    painted: diagnostics::Painted,
    /// Width used to cut the line-end messages on the last layout.
    width: Cell<i32>,
    signature: signature::Help,
    /// The completion popup, made once the cell is selected ([`NotebookTab::complete_in`]).
    completion: RefCell<Option<Rc<Session>>>,
    /// The last snippet a completion put in: its stops are walked while it holds a buffer.
    snippet: RefCell<Option<sourceview5::Snippet>>,
    /// A markdown cell's ghost text, as a note has it.
    ghost: ghost::State,
}

impl CellEditor {
    /// The lines of the text last sent that are this cell's.
    fn lines(&self) -> Option<Range<u32>> {
        let tab = self.tab.upgrade()?;
        let sent = tab.lang.layer(self.kind).sent.borrow();
        sent.program.lines(self.id)
    }

    fn detach(&self) {
        self.signature.dismiss();
        if let Some(session) = self.completion.take() {
            session.close();
        }
    }
}

impl lang::Editor for CellEditor {
    fn view(&self) -> &sourceview5::View {
        &self.view
    }

    fn document(&self) -> Rc<lang::Document> {
        self.doc.clone()
    }

    fn flush(self: Rc<Self>) -> Reply<()> {
        let tab = self.tab.upgrade();
        Box::pin(async move {
            if let Some(tab) = tab {
                tab.flush_layer(self.kind).await;
            }
        })
    }

    fn to_document(&self, at: Pos) -> Option<Pos> {
        Some(Pos {
            line: self.lines()?.start + at.line,
            ..at
        })
    }

    fn to_view(&self, at: Pos) -> Option<Pos> {
        let lines = self.lines()?;
        lines.contains(&at.line).then(|| Pos {
            line: at.line - lines.start,
            ..at
        })
    }

    fn diagnostics(&self) -> Ref<'_, Vec<Diagnostic>> {
        self.painted.items()
    }

    fn language_id(&self) -> String {
        match self.kind {
            Kind::Markdown => "markdown".to_string(),
            _ => self
                .buffer
                .language()
                .map_or_else(|| "text".to_string(), |l| l.id().to_string()),
        }
    }

    fn flavour(&self) -> Flavour {
        super::cell::flavour(self.kind)
    }

    fn is_loading(&self) -> bool {
        self.tab.upgrade().is_some_and(|tab| tab.replaying.get())
    }

    fn signature(&self) -> &signature::Help {
        &self.signature
    }

    fn completion(&self) -> Option<Rc<Session>> {
        self.completion.borrow().clone()
    }

    fn snippet_active(&self) -> bool {
        let snippet = self.snippet.borrow();
        snippet.as_ref().is_some_and(|s| s.buffer().is_some())
    }

    fn push_snippet(&self, snippet: &sourceview5::Snippet, at: &mut gtk::TextIter) {
        self.view.push_snippet(snippet, Some(at));
        self.snippet.replace(Some(snippet.clone()));
    }

    fn ghost(&self) -> Option<&ghost::State> {
        (self.kind == Kind::Markdown).then_some(&self.ghost)
    }
}

fn moved(range: accent_api::Range, line: impl Fn(u32) -> u32) -> accent_api::Range {
    let at = |p: Pos| Pos {
        line: line(p.line),
        ..p
    };
    accent_api::Range {
        start: at(range.start),
        end: at(range.end),
    }
}

/// The server's diagnostics of `program` by the cell they are in, in its lines: one running on
/// past its cell cut at the cell's end, one in no cell dropped, and pyright's warning that a
/// cell's last expression is unused dropped, as a notebook shows that value (user decision
/// 2026-10-20).
fn cells_of(program: &Program, items: Vec<Diagnostic>) -> HashMap<CellId, Vec<Diagnostic>> {
    let mut cells: HashMap<CellId, Vec<Diagnostic>> = HashMap::new();
    for mut item in items {
        let Some((id, _)) = program.cell_at(item.range.start.line) else {
            continue;
        };
        let shown = item.code.as_deref() == Some("reportUnusedExpression")
            && program.ends_cell(item.range.end.line);
        let Some(lines) = program.lines(id).filter(|_| !shown) else {
            continue;
        };
        if !lines.contains(&item.range.end.line) {
            let last = lines.end - 1;
            let text = program.text.lines().nth(last as usize).unwrap_or_default();
            item.range.end = Pos {
                line: last,
                character: text.chars().count() as u32,
            };
        }
        item.range = moved(item.range, |line| line - lines.start);
        cells.entry(id).or_default().push(item);
    }
    cells
}

impl NotebookTab {
    /// Open the code cells' program on `vault`'s language layer, as a code tab's file is: under
    /// the notebook's own path for a marimo notebook, under `<path>.py` for an `.ipynb` (user
    /// decision 2026-10-20), unless no language colours the cells; and the markdown cells as a
    /// note under `<path>.md`. Nothing without a vault.
    pub fn attach_language(self: &Rc<Self>, vault: Option<Arc<Vault>>) {
        if let Some(vault) = vault {
            self.open_layer(Kind::Code, vault.clone(), None);
            self.open_layer(Kind::Markdown, vault, None);
        }
    }

    /// Restart File, and a rename: each document is closed and opened again under the key the
    /// notebook's path gives it now, as it stands.
    pub fn reopen_language(self: &Rc<Self>) {
        for kind in [Kind::Code, Kind::Markdown] {
            let doc = &self.lang.layer(kind).doc;
            if let Some(vault) = doc.vault() {
                self.open_layer(kind, vault, Some(doc.key()));
            }
        }
    }

    /// Open the cells of `kind` as they stand on `vault`, once `closing` is closed there.
    fn open_layer(self: &Rc<Self>, kind: Kind, vault: Arc<Vault>, closing: Option<String>) {
        let (Some(language), Some(key)) = (self.layer_language(kind), self.layer_key(kind)) else {
            return;
        };
        let program = self.text_of(kind);
        let text = program.text.clone();
        let opened = text.clone();
        let layer = self.lang.layer(kind);
        layer.sent.replace(Sent {
            program,
            arrived: false,
        });
        let weak = Rc::downgrade(self);
        let told = move || {
            if let Some(tab) = weak.upgrade() {
                tab.lang.on_starting.emit(&tab);
            }
        };
        let opening = layer.doc.open(vault.clone(), &key, &language, text, told);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            if let Some(old) = closing {
                let _ = vault.close_document(&old).await;
            }
            let opened = match opening.await {
                Ok(support) => {
                    tracing::debug!("opened {key}: {support:?}");
                    Some(opened)
                }
                Err(e) => {
                    tracing::warn!("cannot open {key} on the language layer: {e:#}");
                    None
                }
            };
            // What was typed while it opened.
            if let Some(tab) = weak.upgrade() {
                if let Some(opened) = opened {
                    tab.lang.layer(kind).sent.borrow_mut().arrived(&opened);
                }
                tab.flush_layer(kind).await;
            }
        });
    }

    /// The key the program is open under while it is open on a vault.
    pub fn document_key(&self) -> Option<String> {
        let doc = &self.lang.code.doc;
        doc.vault().map(|_| doc.key())
    }

    /// The document the program is on the language layer.
    pub fn language_document(&self) -> Rc<lang::Document> {
        self.lang.code.doc.clone()
    }

    /// The language the cells of `kind` are opened in: the notebook's own for code, `None` for
    /// code in no language the notebook names.
    fn layer_language(&self, kind: Kind) -> Option<String> {
        match kind {
            Kind::Markdown => Some("markdown".to_string()),
            _ => self.language.as_ref().map(|l| l.id().to_string()),
        }
    }

    /// The key the cells of `kind` open under: for code an `.ipynb`'s path with its code's
    /// extension after it, a server reading a file by its name; for markdown the notebook's path
    /// and `.md`, a note in the notebook's folder, where its relative links are read from, that no
    /// file beside it is.
    fn layer_key(&self, kind: Kind) -> Option<String> {
        let model = self.model.borrow();
        match (kind, model.format()) {
            (Kind::Markdown, _) => Some(format!("{}.md", self.key())),
            (_, Format::Marimo) => Some(self.key()),
            (_, Format::Ipynb) => (!model.extension().is_empty())
                .then(|| format!("{}{}", self.key(), model.extension())),
        }
    }

    /// The notebook changed: the server's copy of either text may be stale, which a flush a
    /// moment after the last change finds out; a run's outputs leave both as they were, and send
    /// nothing. As in a note, the markdown cell typed in asks for its ghost text first.
    pub(super) fn language_changed(self: &Rc<Self>) {
        let docs = [&self.lang.code.doc, &self.lang.prose.doc];
        if docs.iter().all(|doc| doc.vault().is_none()) {
            return;
        }
        for doc in docs {
            doc.edited();
        }
        // A signature that is up is about the call being typed, so it is asked again.
        let cells: Vec<Rc<CellEditor>> = self.lang.editors.borrow().values().cloned().collect();
        for cell in cells.into_iter().filter(|c| c.signature.is_shown()) {
            signature::request(&(cell as Rc<dyn lang::Editor>));
        }
        let weak = Rc::downgrade(self);
        let handle = glib::spawn_future_local(async move {
            glib::timeout_future(lang::GHOST).await;
            if let Some(tab) = weak.upgrade() {
                tab.suggest().await;
            }
            glib::timeout_future(lang::REFRESH.saturating_sub(lang::GHOST)).await;
            if let Some(tab) = weak.upgrade() {
                tab.flush_language().await;
            }
        });
        if let Some(old) = self.lang.refresh.replace(Some(handle)) {
            old.abort();
        }
    }

    /// The selected cell's ghost text, if it is a markdown cell.
    async fn suggest(self: Rc<Self>) {
        let cell = self.selected.get().and_then(|id| {
            let editors = self.lang.editors.borrow();
            editors
                .get(&id)
                .filter(|c| c.kind == Kind::Markdown)
                .cloned()
        });
        if let Some(cell) = cell {
            self.flush_layer(Kind::Markdown).await;
            ghost::request(cell.as_ref()).await;
        }
    }

    /// Send the server both texts as they stand.
    async fn flush_language(self: Rc<Self>) {
        self.clone().flush_layer(Kind::Code).await;
        self.flush_layer(Kind::Markdown).await;
    }

    /// Send the server the cells of `kind` as they stand, if that is not the text it holds.
    /// Dropped part way or refused, it leaves the text unsent, and the next flush sends it again.
    async fn flush_layer(self: Rc<Self>, kind: Kind) {
        let doc = self.lang.layer(kind).doc.clone();
        let handed = RefCell::new(None);
        let sent = doc
            .flush(|| {
                let program = self.text_of(kind);
                let text = self.lang.layer(kind).sent.borrow_mut().next(program);
                handed.replace(text.clone());
                text
            })
            .await;
        if let (true, Some(text)) = (sent, handed.take()) {
            self.lang.layer(kind).sent.borrow_mut().arrived(&text);
        }
    }

    /// The cells of `kind` as the model holds them.
    fn text_of(&self, kind: Kind) -> Program {
        self.sync_model();
        let model = self.model.borrow();
        match kind {
            Kind::Markdown => model.prose(),
            _ => model.program(),
        }
    }

    /// The server saved the document: what it does on a save of a code tab's file. The markdown
    /// cells' note has no file to be read again.
    pub fn language_saved(self: &Rc<Self>) {
        let Some(vault) = self.lang.code.doc.vault() else {
            return;
        };
        let tab = self.clone();
        glib::spawn_future_local(async move {
            tab.clone().flush_layer(Kind::Code).await;
            let key = tab.lang.code.doc.key();
            if let Err(e) = vault.save_document(&key).await {
                tracing::debug!("saved {key}: {e:#}");
            }
        });
    }

    /// The link to the vault is back: what was typed while it was down is sent.
    pub fn resync_language(self: &Rc<Self>) {
        glib::spawn_future_local(self.clone().flush_language());
    }

    /// The Ghost Text preference, which the markdown cells follow as a note does: off takes a
    /// suggestion away at once.
    pub fn set_ghost_text(&self, on: bool) {
        self.lang.ghost.set(on);
        for cell in self.lang.editors.borrow().values() {
            if let Some(state) = lang::Editor::ghost(cell.as_ref()) {
                state.on.set(on);
                if !on {
                    ghost::clear(cell.as_ref());
                }
            }
        }
    }

    /// The kind of cells whose document is open under `key`, if it is one of the notebook's.
    fn kind_of(&self, key: &str) -> Option<Kind> {
        [Kind::Code, Kind::Markdown].into_iter().find(|&kind| {
            let doc = &self.lang.layer(kind).doc;
            doc.vault().is_some() && doc.key() == key
        })
    }

    /// Whether `key` is one of the notebook's documents on the language layer: the program or
    /// the markdown cells' note.
    pub fn has_document(&self, key: &str) -> bool {
        self.kind_of(key).is_some()
    }

    /// A publish for the document open under `key`: each of its cells' share painted in the
    /// cell's editor, or kept for when it has one.
    pub fn set_diagnostics(&self, key: &str, items: Vec<Diagnostic>) {
        let Some(kind) = self.kind_of(key) else {
            return;
        };
        let layer = self.lang.layer(kind);
        let mut cells = cells_of(&layer.sent.borrow().program, items);
        let editors: Vec<Rc<CellEditor>> = self.lang.editors.borrow().values().cloned().collect();
        for cell in editors.into_iter().filter(|c| c.kind == kind) {
            cell.painted.set(cells.remove(&cell.id).unwrap_or_default());
        }
        *layer.waiting.borrow_mut() = cells;
    }

    /// The index found or lost files: the markdown cells' links are diagnosed again, as an open
    /// note's are, since a link may name a note that is there now.
    pub fn rediagnose(&self) {
        let doc = &self.lang.prose.doc;
        let Some(vault) = doc.vault() else {
            return;
        };
        let key = doc.key();
        glib::spawn_future_local(async move {
            if let Err(e) = vault.rediagnose(&key).await {
                tracing::debug!("re-diagnosing {key}: {e:#}");
            }
        });
    }

    /// Recut each built cell's full diagnostic messages after its width or font changes.
    pub(super) fn refit_diagnostics(&self, font_changed: bool) {
        for cell in self.lang.editors.borrow().values() {
            let width = cell.view.visible_rect().width();
            if (cell.width.replace(width) != width || font_changed)
                && !cell.painted.messages().is_empty()
            {
                cell.painted.paint();
            }
        }
    }

    /// The status bar's errors and warnings over every cell, as a code tab counts its own.
    pub fn diagnostics_counts(&self) -> Option<String> {
        let mut all: Vec<Diagnostic> = Vec::new();
        for layer in [&self.lang.code, &self.lang.prose] {
            all.extend(layer.waiting.borrow().values().flatten().cloned());
        }
        for cell in self.lang.editors.borrow().values() {
            all.extend(cell.painted.items().iter().cloned());
        }
        diagnostics::counts(&all)
    }

    /// Take the diagnostics out of every cell's text, or put them back, as a code tab's count's
    /// press does.
    pub fn hide_diagnostics(&self, hidden: bool) {
        self.lang.hidden.set(hidden);
        for cell in self.lang.editors.borrow().values() {
            cell.painted.hide(hidden);
        }
    }

    pub fn diagnostics_hidden(&self) -> bool {
        self.lang.hidden.get()
    }

    /// Give cell `view`'s editor, just built, its keys and the language features over its
    /// document, with the diagnostics that waited for it, and Go to Definition by Ctrl+click: a
    /// code cell a code tab's; a markdown cell a note's keys, hover, completion and ghost text, its
    /// Ctrl+click following a link.
    pub(super) fn attach_cell(self: &Rc<Self>, view: &CellView, editor: &Editor) {
        if !matches!(view.kind, Kind::Code | Kind::Markdown) {
            return;
        }
        let painted = diagnostics::Painted::new(&editor.view, &editor.buffer);
        painted.hide(self.lang.hidden.get());
        let layer = self.lang.layer(view.kind);
        let waiting = layer.waiting.borrow_mut().remove(&view.id);
        painted.set(waiting.unwrap_or_default());
        let cell = Rc::new(CellEditor {
            id: view.id,
            kind: view.kind,
            tab: Rc::downgrade(self),
            doc: self.lang.layer(view.kind).doc.clone(),
            view: editor.view.clone(),
            buffer: editor.buffer.clone(),
            painted,
            width: Cell::new(0),
            signature: signature::Help::default(),
            completion: RefCell::default(),
            snippet: RefCell::default(),
            ghost: ghost::State::default(),
        });
        follow_edits(&cell);
        let features: Rc<dyn lang::Editor> = cell.clone();
        crate::editor::attach_keys(&features);
        crate::hover::install(&features);
        match view.kind {
            Kind::Code => crate::signature::install(&features),
            _ => {
                cell.ghost.on.set(self.lang.ghost.get());
                ghost::install(&features);
            }
        }
        self.follow_clicks(&editor.view);
        self.lang.editors.borrow_mut().insert(view.id, cell);
        if self.selected.get() == Some(view.id) {
            self.complete_in(view.id);
        }
    }

    /// Ctrl+click in `view` is Go to Definition at the place clicked, as in a text tab: a code
    /// cell's name, a markdown cell's link.
    fn follow_clicks(self: &Rc<Self>, view: &sourceview5::View) {
        // Claimed in the capture phase, before the view's own click and drag gestures see the
        // press, as a text tab's is: they would start a selection that the release stretches
        // from where the jump landed to wherever the pointer then is.
        let click = gtk::GestureClick::builder()
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |gesture, _, x, y| {
            let ctrl = gesture
                .current_event_state()
                .contains(crate::platform::PRIMARY_MODIFIER);
            let view = gesture.widget().and_downcast::<sourceview5::View>();
            let (true, Some(tab), Some(view)) = (ctrl, weak.upgrade(), view) else {
                return;
            };
            // Only a press on the text, the caret going there first.
            let (bx, by) =
                view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
            let Some(iter) = crate::fold::iter_at_location(&view, bx, by) else {
                return;
            };
            view.buffer().place_cursor(&iter);
            gesture.set_state(gtk::EventSequenceState::Claimed);
            // The view's own press would have taken the keyboard.
            view.grab_focus();
            tab.lang.on_follow.emit(&tab);
        });
        view.add_controller(click);
    }

    /// Cell `id` was selected: its editor gets its completion popup, if it has none yet. Made
    /// for the cell typed in rather than for each one built, a long notebook building many, and
    /// in an idle: a selection can come from a focus event, inside which no widget is parented.
    pub(super) fn complete_in(&self, id: CellId) {
        let cell = self.lang.editors.borrow().get(&id).map(Rc::downgrade);
        let Some(cell) = cell else {
            return;
        };
        glib::idle_add_local_once(move || {
            let Some(cell) = cell.upgrade() else { return };
            if cell.completion.borrow().is_some() || cell.doc.vault().is_none() {
                return;
            }
            let features: Rc<dyn lang::Editor> = cell.clone();
            let source = Box::new(EditorSource(Rc::downgrade(&features)));
            let session = crate::completion::install(&features, source);
            cell.completion.replace(Some(session));
        });
    }

    /// Cell `id`'s view is going, its editor with it.
    pub(super) fn detach_cell(&self, id: CellId) {
        let cell = self.lang.editors.borrow_mut().remove(&id);
        if let Some(cell) = cell {
            cell.detach();
        }
    }

    /// Whether cell `id`'s completion popup is on screen.
    pub(super) fn popup_shown(&self, id: CellId) -> bool {
        let editors = self.lang.editors.borrow();
        let session = editors
            .get(&id)
            .and_then(|cell| cell.completion.borrow().clone());
        session.is_some_and(|s| s.is_shown())
    }

    /// The theme changed: the cells' underlines take its colours.
    pub(super) fn restyle_language(&self) {
        for cell in self.lang.editors.borrow().values() {
            diagnostics::restyle(&cell.buffer, &cell.view);
        }
    }

    /// The selected cell's editor on the language layer, built if need be: what Quick Fix asks
    /// about, and Go to Definition in a code cell.
    pub(crate) fn language_editor(self: &Rc<Self>) -> Option<Rc<dyn lang::Editor>> {
        Some(self.language_editor_of(self.selected.get()?)?)
    }

    /// Cell `id`'s editor on the language layer, built if need be.
    fn language_editor_of(self: &Rc<Self>, id: CellId) -> Option<Rc<CellEditor>> {
        self.editor(id)?;
        self.lang.editors.borrow().get(&id).cloned()
    }

    /// The caret at `at` in the program, in the cell that line is in, a quarter down the view.
    pub fn go_to(self: &Rc<Self>, at: Pos) -> bool {
        let found = self.lang.code.sent.borrow().program.cell_at(at.line);
        let Some((id, line)) = found else {
            return false;
        };
        let Some((_, editor)) = self.editor(id) else {
            return false;
        };
        let iter = lang::iter_at(&editor.buffer, Pos { line, ..at });
        self.goto(id, iter.offset());
        true
    }

    /// Go to a position in a marimo file, whose wrapper lines are outside the cell program.
    pub fn go_to_file(self: &Rc<Self>, at: Pos) -> bool {
        let found = (|| {
            let model = self.model.borrow();
            (model.format() == Format::Marimo).then_some(())?;
            let text = model.to_text().replace("\r\n", "\n");
            let byte = accent_api::language::byte_of(&text, at)?;
            let (cell, offset) = model.place(byte)?;
            let cell = &model.cells()[cell];
            let chars = cell.source()[..offset].chars().count() as i32;
            Some((cell.id(), chars))
        })();
        let Some((id, chars)) = found else {
            return false;
        };
        self.goto(id, chars);
        true
    }

    /// Called for a Ctrl+click in a code cell.
    pub fn connect_follow(&self, f: impl Fn(&Rc<NotebookTab>) + 'static) {
        self.lang.on_follow.set(Rc::new(f));
    }

    /// Called when the document starts or stops waiting on its server: see
    /// [`lang::Document::starting`].
    pub fn connect_starting(&self, f: impl Fn(&Rc<NotebookTab>) + 'static) {
        self.lang.on_starting.set(Rc::new(f));
    }
}

/// What `ACCENT_BENCH_NOTEBOOK_LSP` reads.
#[cfg(feature = "bench")]
impl NotebookTab {
    /// Whether the program's server and the notes answered their openings.
    pub fn bench_supported(&self) -> bool {
        self.lang.code.doc.support().is_some() && self.lang.prose.doc.support().is_some()
    }

    /// The key the markdown cells are open under.
    pub fn bench_prose_key(&self) -> String {
        self.lang.prose.doc.key()
    }

    /// Cell `n`'s editor on the language layer, built if need be.
    pub fn bench_language_editor(self: &Rc<Self>, n: usize) -> Option<Rc<dyn lang::Editor>> {
        let id = self.views.borrow().get(n)?.id;
        Some(self.language_editor_of(id)?)
    }

    /// The selected cell, by its place, and its caret's line and column.
    pub fn bench_caret(self: &Rc<Self>) -> Option<(usize, i32, i32)> {
        let id = self.selected.get()?;
        let (_, editor) = self.editor(id)?;
        let caret = editor.buffer.iter_at_mark(&editor.buffer.get_insert());
        Some((self.position_of(id)?, caret.line(), caret.line_offset()))
    }

    /// Each code cell's diagnostics, and the lines its editor underlines as errors, `None` for
    /// one not built.
    pub fn bench_diagnostics(&self) -> Vec<(Vec<Diagnostic>, Option<Vec<i32>>)> {
        let ids: Vec<CellId> = self
            .views
            .borrow()
            .iter()
            .filter(|v| v.kind == Kind::Code)
            .map(|v| v.id)
            .collect();
        let (editors, waiting) = (self.lang.editors.borrow(), self.lang.code.waiting.borrow());
        ids.into_iter()
            .map(|id| match editors.get(&id) {
                Some(cell) => (
                    cell.painted.items().clone(),
                    Some(diagnostics::error_lines(&cell.buffer).0),
                ),
                None => (waiting.get(&id).cloned().unwrap_or_default(), None),
            })
            .collect()
    }
}

/// Keep `cell`'s diagnostics on their text as it is typed into, as a code tab's are; a buffer
/// set from the model or the history leaves them where they are until the next publish.
fn follow_edits(cell: &Rc<CellEditor>) {
    let weak = Rc::downgrade(cell);
    cell.buffer.connect_insert_text(move |_, at, text| {
        if let Some(cell) = weak.upgrade().filter(|c| !lang::Editor::is_loading(&**c)) {
            cell.painted.shift(at, at, text);
        }
    });
    let weak = Rc::downgrade(cell);
    cell.buffer.connect_delete_range(move |_, from, to| {
        if let Some(cell) = weak.upgrade().filter(|c| !lang::Editor::is_loading(&**c)) {
            cell.painted.shift(from, to, "");
        }
    });
    let weak = Rc::downgrade(cell);
    cell.buffer.connect_changed(move |buffer| {
        if let Some(cell) = weak.upgrade().filter(|c| !lang::Editor::is_loading(&**c)) {
            cell.painted.edited(crate::editor::caret(buffer).line());
        }
    });
    // The underlines' colours are read off the view's, which it has once it is mapped: at once
    // for an editor built in a cell on screen, which is mapped as it goes in.
    if cell.view.is_mapped() {
        diagnostics::restyle(&cell.buffer, &cell.view);
    }
    let weak = Rc::downgrade(cell);
    cell.view.connect_map(move |view| {
        if let Some(cell) = weak.upgrade() {
            diagnostics::restyle(&cell.buffer, view);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use accent_api::Severity;
    use accent_core::notebook::{Edit, read};

    fn range(line: u32, from: u32, to_line: u32, to: u32) -> accent_api::Range {
        accent_api::Range {
            start: Pos {
                line,
                character: from,
            },
            end: Pos {
                line: to_line,
                character: to,
            },
        }
    }

    fn diagnostic(at: accent_api::Range, code: Option<&str>) -> Diagnostic {
        Diagnostic {
            range: at,
            severity: Severity::Warning,
            message: "here".to_string(),
            source: None,
            code: code.map(str::to_string),
        }
    }

    /// Three code cells, `import os\nx = 1` (lines 0-1), `y = x + oops` (2) and `x` (3), under
    /// a markdown one.
    fn program() -> (Program, Vec<CellId>) {
        let mut nb = read("t.ipynb", accent_core::notebook::EMPTY_IPYNB).unwrap();
        let first = nb.cells()[0].id();
        nb.apply(Edit::Source {
            id: first,
            text: "import os\nx = 1".into(),
        });
        for (at, kind, source) in [
            (0, Kind::Markdown, "# T"),
            (2, Kind::Code, "y = x + oops"),
            (3, Kind::Code, "x"),
        ] {
            let cell = nb.new_cell(kind, source).unwrap();
            nb.apply(Edit::Insert { at, cell });
        }
        let ids = nb
            .cells()
            .iter()
            .filter(|c| c.kind() == Kind::Code)
            .map(|c| c.id())
            .collect();
        (nb.program(), ids)
    }

    #[test]
    fn diagnostics_land_in_their_cells_and_a_shown_value_is_not_unused() {
        let (program, ids) = program();
        let cells = cells_of(
            &program,
            vec![
                diagnostic(range(2, 8, 2, 12), Some("reportUndefinedVariable")),
                // The value the last cell shows, and a run-on that leaves its cell.
                diagnostic(range(3, 0, 3, 1), Some("reportUnusedExpression")),
                diagnostic(range(1, 4, 2, 3), None),
                diagnostic(range(9, 0, 9, 1), None),
            ],
        );
        let at = |id: &CellId| {
            let items = cells.get(id).cloned().unwrap_or_default();
            let ends = |r: accent_api::Range| {
                (r.start.line, r.start.character, r.end.line, r.end.character)
            };
            items.into_iter().map(|d| ends(d.range)).collect::<Vec<_>>()
        };
        assert_eq!(at(&ids[0]), [(1, 4, 1, 5)], "cut at its cell's end");
        assert_eq!(at(&ids[1]), [(0, 8, 0, 12)], "in the cell's own lines");
        assert_eq!(at(&ids[2]), [], "a shown value is not unused");
        assert_eq!(cells.len(), 2, "nothing for a line in no cell");
    }

    #[test]
    fn a_program_handed_over_is_sent_again_until_it_arrives() {
        let (program, _) = program();
        let mut sent = Sent::default();
        assert_eq!(sent.next(program.clone()), Some(program.text.clone()));
        assert!(
            sent.next(program.clone()).is_some(),
            "a flush dropped part way may have sent nothing"
        );
        sent.arrived("an older text");
        assert!(sent.next(program.clone()).is_some(), "not this program");
        sent.arrived(&program.text);
        assert_eq!(sent.next(program.clone()), None, "the server holds it");
        assert!(sent.next(Program::default()).is_some(), "an edit is sent");
    }
}
