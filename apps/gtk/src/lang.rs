//! What the editor knows about a language server's answers.
//!
//! One tab, one document on the vault's language layer. [`attach`] opens it and installs the
//! providers that ask questions about it; [`changed`] keeps the server's copy in step with the
//! buffer and re-reads what an edit invalidates; [`detach`] closes it.
//!
//! Nothing here blocks: every request is a `Task` awaited with `glib::spawn_future_local`, and
//! dropping the future cancels the request at the server. The refresh after an edit is one
//! coalescing rule for a local vault and a remote one alike — the latest edit wins, and a burst
//! of keystrokes costs one round trip rather than one per key.

use crate::editor::{Flavour, Tab};
use accent_api::{Kind, Pos, Support, Symbol, Vault};
use futures_channel::oneshot;
use gtk::glib;
use gtk::prelude::*;
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// DESIGN.md, Motion: the symbols and folds behind the Outline pane, the sticky title and the
/// gutter chevrons follow the last edit by 300 ms, as the preview does. The ghost text comes
/// first and sooner, after [`GHOST`]; the rest of the wait is served after it.
const REFRESH: Duration = Duration::from_millis(300);

/// How long after the last keystroke ghost text is asked for. Short, because it is painted at
/// the caret, where a wait is read as the suggestion being gone; the answer itself costs a
/// fraction of a millisecond.
const GHOST: Duration = Duration::from_millis(100);

/// A callback the window registered, in the shape `editor.rs` uses for its own.
pub type Hook = Rc<dyn Fn(&Rc<Tab>)>;

/// What the language layer hands back to the window when an answer arrives.
pub struct Hooks {
    /// New symbols for this tab: the Outline pane and the sticky title are drawn from them.
    pub on_symbols: Hook,
}

/// Everything one tab knows about its document on the language layer.
///
/// A field on [`Tab`] rather than a map keyed by path: a tab is what opens a document and what
/// closes it, and two tabs on one file would otherwise share one entry.
#[derive(Default)]
pub struct State {
    /// What the provider said it could do, once the document was open. `None` until then.
    /// Shared rather than copied: every keystroke asks it whether the character just typed opens
    /// a popup or a signature, and the answer carries two vectors and a string.
    support: RefCell<Option<Rc<Support>>>,
    /// The document's symbols, most recent answer.
    symbols: RefCell<Vec<Symbol>>,
    /// The same symbols as the Outline pane lists them, flattened once per answer rather than
    /// once per caret move.
    rows: RefCell<Vec<Row>>,
    /// The pending post-edit refresh. Replaced rather than queued, so the latest edit wins.
    refresh: RefCell<Option<glib::JoinHandle<()>>>,
    /// The buffer's own edit counter, and how far the server has been told. A bool could only
    /// say "someone is going to send this", never "the text the server holds is that one", so a
    /// request arriving while a flush was in flight read it as clean and asked the server about
    /// characters it had not been given.
    version: Cell<u64>,
    sent: Cell<u64>,
    /// The callers waiting out the flush in flight, `None` while there is none: the next caller
    /// waits for it rather than starting a second one.
    waiting: RefCell<Option<Vec<oneshot::Sender<()>>>>,
    /// The "no language server" toast has been said for this tab; it is not said again.
    toasted: Cell<bool>,
    /// The vault this document is open on. `None` for a tab outside every vault, which has
    /// nobody to ask and gets no providers.
    vault: RefCell<Option<Arc<Vault>>>,
    hooks: RefCell<Option<Rc<Hooks>>>,
    /// The signature popover of this tab, and the request that would fill it.
    pub signature: crate::signature::Help,
    /// Whether ghost text is wanted here and whether anything is standing in its way.
    pub ghost: crate::ghost::State,
}

impl State {
    /// The vault to ask, for a tab that has one.
    pub fn vault(&self) -> Option<Arc<Vault>> {
        self.vault.borrow().clone()
    }

    /// What the provider can do; `None` while the document is still opening.
    pub fn support(&self) -> Option<Rc<Support>> {
        self.support.borrow().clone()
    }

    pub fn symbols(&self) -> Vec<Symbol> {
        self.symbols.borrow().clone()
    }

    /// The Outline pane's rows, as the pane takes them: how deep, what name, where a click lands.
    pub fn outline(&self) -> Vec<(u8, String, Pos)> {
        self.rows
            .borrow()
            .iter()
            .map(|row| (row.depth, row.name.clone(), row.at))
            .collect()
    }

    /// The Outline row of the symbol a caret on `line` is in: see [`row_at`].
    pub fn outline_row(&self, line: u32) -> Option<usize> {
        row_at(&self.rows.borrow(), line)
    }

    /// Whether a caret on `line` is before every symbol: above a note's first heading.
    pub fn above_outline(&self, line: u32) -> bool {
        self.rows
            .borrow()
            .first()
            .is_some_and(|row| line < row.lines.0)
    }

    /// Whether the "no language server" toast still has to be said, marking it said.
    pub fn claim_toast(&self) -> bool {
        !self.toasted.replace(true)
    }
}

/// Where an iter is, in the coordinates every request and answer uses: a zero-based line and a
/// column counted in characters, which is exactly what `TextIter` reports.
pub fn pos_of(iter: &gtk::TextIter) -> Pos {
    Pos {
        line: iter.line().max(0) as u32,
        character: iter.line_offset().max(0) as u32,
    }
}

/// The iter at `pos`, the inverse of [`pos_of`], clamped to what the buffer actually has: an
/// answer can outlive the edit that shortened the line it was about, and an out-of-range offset is
/// a GTK critical.
pub fn iter_at(buffer: &impl IsA<gtk::TextBuffer>, pos: Pos) -> gtk::TextIter {
    let end = crate::editor::line_end(buffer, pos.line as i32);
    let mut iter = end;
    iter.set_line_offset((pos.character as i32).clamp(0, end.line_offset()));
    iter
}

/// The LSP language id for a tab: the GtkSourceView language's own id, which is the same name
/// (`rust`, `c`, `python`), and `markdown` for a note, whose buffer carries no language because
/// our own styling pass does that job.
pub fn language_id(tab: &Tab) -> String {
    match tab.flavour() {
        Flavour::Note => "markdown".to_string(),
        // No language at all is `text`, which is what the word suggestions are keyed on.
        _ => tab
            .buffer
            .language()
            .map_or_else(|| "text".to_string(), |l| l.id().to_string()),
    }
}

/// Open this tab's document on `vault` and wire the providers that read it.
///
/// A CSV is skipped: its columns are coloured by us and no language server speaks the format, so
/// opening it would only cost a round trip to be told nothing.
///
/// A file outside every vault (`None`) has no language layer to ask: a note there is outlined from
/// its own text ([`outline_alone`]), and anything else gets nothing.
pub fn attach(tab: &Rc<Tab>, vault: Option<Arc<Vault>>, hooks: Hooks) {
    if tab.flavour() == Flavour::Csv {
        return;
    }
    *tab.lang.hooks.borrow_mut() = Some(Rc::new(hooks));
    let Some(vault) = vault else {
        return restart(tab, Duration::ZERO);
    };
    *tab.lang.vault.borrow_mut() = Some(vault.clone());
    crate::completion::install(tab);
    crate::hover::install(tab);
    crate::signature::install(tab);
    crate::ghost::install(tab);

    let (rel, id, text) = (tab.rel(), language_id(tab), tab.text());
    let weak = Rc::downgrade(tab);
    glib::spawn_future_local(async move {
        let support = vault.open_document(&rel, &id, text).await;
        let Some(tab) = weak.upgrade() else { return };
        match support {
            Ok(support) => {
                tracing::debug!("opened {rel} as {id}: {support:?}");
                *tab.lang.support.borrow_mut() = Some(Rc::new(support));
                restart(&tab, Duration::ZERO);
            }
            Err(e) => tracing::warn!("cannot open {rel} on the language layer: {e:#}"),
        }
    });
}

/// The buffer changed at all: the next request sends the text before it asks. Said on every
/// edit, ahead of [`changed`], which a note over 16 K characters hears only once the typing
/// pauses — a Go to Definition inside that pause asked about the text before it.
pub fn edited(tab: &Tab) {
    tab.lang.version.set(tab.lang.version.get().wrapping_add(1));
}

/// The buffer changed: the server's copy is stale and everything derived from it is too.
pub fn changed(tab: &Rc<Tab>) {
    edited(tab);
    // A signature that is up is about the call being typed, so it is asked again rather than
    // left saying what the last keystroke meant.
    if tab.lang.signature.is_shown() {
        crate::signature::request(tab);
    }
    restart(tab, REFRESH);
}

/// The buffer reached the disk. The server hears the edit first, then the save: rust-analyzer
/// runs `cargo check` on a save and nothing else, so without this a Rust file never gets its
/// compiler diagnostics.
pub fn saved(tab: &Rc<Tab>) {
    let Some(vault) = tab.lang.vault() else {
        return;
    };
    let tab = tab.clone();
    glib::spawn_future_local(async move {
        flush(tab.clone()).await;
        let rel = tab.rel();
        if let Err(e) = vault.save_document(&rel).await {
            tracing::debug!("saved {rel}: {e:#}");
        }
    });
}

/// The user left this document. A provider too expensive to tell about every save hears about
/// it here instead: ghost text re-reads the vault on a save, which is a second's work on a large
/// one and not something to do a second after every keystroke.
pub fn settle(tab: &Rc<Tab>) {
    let Some(vault) = tab.lang.vault() else {
        return;
    };
    let tab = tab.clone();
    glib::spawn_future_local(async move {
        flush(tab.clone()).await;
        let rel = tab.rel();
        if let Err(e) = vault.settle(&rel).await {
            tracing::debug!("settling {rel}: {e:#}");
        }
    });
}

/// The index moved: ask for this document's hints again. A note flags a `[[link]]` it cannot
/// resolve, and creating the note it names is not an edit of *this* one, so without this the
/// hint stays until the next keystroke.
pub fn rediagnose(tab: &Rc<Tab>) {
    let Some(vault) = tab.lang.vault() else {
        return;
    };
    let rel = tab.rel();
    glib::spawn_future_local(async move {
        if let Err(e) = vault.rediagnose(&rel).await {
            tracing::debug!("re-diagnosing {rel}: {e:#}");
        }
    });
}

/// Close the document and drop whatever is still in flight for it. Called from `Tab::drop`, so
/// the future it spawns holds the vault handle and the path and nothing else.
pub fn detach(tab: &Tab) {
    tab.lang.signature.dismiss();
    if let Some(handle) = tab.lang.refresh.borrow_mut().take() {
        handle.abort();
    }
    let Some(vault) = tab.lang.vault.borrow_mut().take() else {
        return;
    };
    let rel = tab.rel();
    glib::spawn_future_local(async move {
        tracing::debug!("closing {rel}");
        if let Err(e) = vault.close_document(&rel).await {
            tracing::debug!("closing {rel}: {e:#}");
        }
    });
}

/// A rename landed: the old path is closed and the new one opened, because a language server
/// keys its documents by URI and knows nothing of the move.
pub fn retarget(tab: &Rc<Tab>, old_rel: &str) {
    let Some(vault) = tab.lang.vault() else {
        return;
    };
    let old = old_rel.to_string();
    let (rel, id, text) = (tab.rel(), language_id(tab), tab.text());
    let weak = Rc::downgrade(tab);
    glib::spawn_future_local(async move {
        let _ = vault.close_document(&old).await;
        let support = vault.open_document(&rel, &id, text).await;
        let Some(tab) = weak.upgrade() else { return };
        if let Ok(support) = support {
            *tab.lang.support.borrow_mut() = Some(Rc::new(support));
            restart(&tab, Duration::ZERO);
        }
    });
}

/// The server behind this tab was replaced. The reconnect reopened the document with the last
/// text the tab tried to send, and an edit refused while that connection was still being made is
/// newer: refreshing now sends it and re-reads what the new server says, rather than leaving both
/// to the next keystroke.
pub fn resync(tab: &Rc<Tab>) {
    restart(tab, Duration::ZERO);
}

/// Send the pending edit and wait for the server to have it. Every positional request awaits
/// this first: an answer about a text the server has not been given is an answer about the wrong
/// characters.
///
/// Two callers meeting here — the completion, the hover, the signature and the ghost text all
/// flush before they ask — wait for one round trip between them, and a change that fails leaves
/// the version unsent so the next caller carries it again.
pub async fn flush(tab: Rc<Tab>) {
    // Looked at again once woken: another waiter may have started the next flush by then.
    loop {
        let woken = match tab.lang.waiting.borrow_mut().as_mut() {
            Some(waiting) => {
                let (wake, woken) = oneshot::channel();
                waiting.push(wake);
                woken
            }
            None => break,
        };
        // A send or a dropped sender: either way the flush it waited for is over.
        let _ = woken.await;
    }
    let Some(vault) = tab.lang.vault() else {
        return;
    };
    let wanted = tab.lang.version.get();
    if tab.lang.sent.get() >= wanted {
        return;
    }
    let (rel, text) = (tab.rel(), tab.text());
    tracing::debug!("changed {rel}, {} chars", text.chars().count());
    let flushing = Flushing::new(tab.clone());
    let sent = vault.change_document(&rel, text).await;
    drop(flushing);
    match sent {
        Ok(()) => tab.lang.sent.set(tab.lang.sent.get().max(wanted)),
        Err(e) => tracing::debug!("changing {rel}: {e:#}"),
    }
}

/// Marks a flush as in flight for as long as it lives, and wakes whoever waited for it as it
/// goes — a dropped future included, which is what the next keystroke does to the refresh a flush
/// may be running inside. Without the drop the mark would outlive the request and every later
/// flush would wait for a round trip that is no longer happening.
struct Flushing(Rc<Tab>);

impl Flushing {
    fn new(tab: Rc<Tab>) -> Self {
        *tab.lang.waiting.borrow_mut() = Some(Vec::new());
        Flushing(tab)
    }
}

impl Drop for Flushing {
    fn drop(&mut self) {
        for wake in self.0.lang.waiting.take().into_iter().flatten() {
            // A waiter dropped meanwhile is not there to hear it, and needs nothing.
            let _ = wake.send(());
        }
    }
}

/// Re-arm the post-edit refresh, dropping whatever was pending. Aborting the old future is what
/// cancels its requests at the server, so a fast typist leaves one in flight rather than one per
/// keystroke.
fn restart(tab: &Rc<Tab>, delay: Duration) {
    if let Some(handle) = tab.lang.refresh.borrow_mut().take() {
        handle.abort();
    }
    let weak = Rc::downgrade(tab);
    let handle = glib::spawn_future_local(async move {
        pause(delay.min(GHOST)).await;
        let Some(tab) = weak.upgrade() else { return };
        refresh(tab, delay.saturating_sub(GHOST)).await;
    });
    *tab.lang.refresh.borrow_mut() = Some(handle);
}

/// Wait, unless there is nothing to wait for: a zero-length timeout still costs a turn of the
/// main loop, and the callers that pass no delay want the answers now.
async fn pause(delay: Duration) {
    if !delay.is_zero() {
        glib::timeout_future(delay).await;
    }
}

/// Give the server the edit, then re-read what it implies: the symbols the Outline pane and the
/// sticky title are drawn from, and the blocks that can be folded.
///
/// One future for both halves, so the next keystroke's abort still cancels either. `rest` is
/// what is left of the wait once the ghost has been asked for.
async fn refresh(tab: Rc<Tab>, rest: Duration) {
    let Some(vault) = tab.lang.vault() else {
        return outline_alone(tab, rest).await;
    };
    let edits = tab.save.edits.get();
    flush(tab.clone()).await;
    // First, because it is the one answer the user is waiting to see: the symbols and folds
    // behind it feed panes that are already drawn.
    crate::ghost::request(&tab).await;
    pause(rest).await;
    let rel = tab.rel();
    match vault.symbols(&rel).await {
        Ok(symbols) => {
            *tab.lang.rows.borrow_mut() = flatten(&symbols);
            *tab.lang.symbols.borrow_mut() = symbols;
        }
        Err(e) => tracing::debug!("symbols for {rel}: {e:#}"),
    }
    match vault.folds(&rel).await {
        // Folds laid over a text they were not worked out for hide the wrong lines, and a long
        // note tells this layer of an edit only after the editor's debounce: an answer landing
        // after an edit made since this refresh began is left to that edit's own refresh.
        Ok(folds) if tab.save.edits.get() == edits => tab.set_folds(folds),
        Ok(_) => {}
        Err(e) => tracing::debug!("folds for {rel}: {e:#}"),
    }
    let hooks = tab.lang.hooks.borrow().clone();
    if let Some(hooks) = hooks {
        (hooks.on_symbols)(&tab);
    }
}

/// [`refresh`] for a note outside every vault: its headings and folds are read from its own text,
/// off the main loop, as the notes provider reads them inside one.
async fn outline_alone(tab: Rc<Tab>, rest: Duration) {
    if !tab.flavour().is_note() {
        return;
    }
    pause(rest).await;
    let (edits, text) = (tab.save.edits.get(), tab.text());
    let outline =
        crate::work::off_thread("outline", move || accent_api::language::note_outline(&text));
    let Some((symbols, folds)) = outline.await else {
        return;
    };
    *tab.lang.rows.borrow_mut() = flatten(&symbols);
    *tab.lang.symbols.borrow_mut() = symbols;
    // As in `refresh`: folds worked out for a text edited since would hide the wrong lines.
    if tab.save.edits.get() == edits {
        tab.set_folds(folds);
    }
    let hooks = tab.lang.hooks.borrow().clone();
    if let Some(hooks) = hooks {
        (hooks.on_symbols)(&tab);
    }
}

/// One row of the Outline pane: a symbol lifted out of the tree.
struct Row {
    /// 1 for a top-level symbol, which is what the pane indents from.
    depth: u8,
    name: String,
    /// Where a click on the row lands.
    at: Pos,
    /// The first and the last line of the whole symbol: the section under a heading, the body
    /// of a function.
    lines: (u32, u32),
    /// The row of the symbol this one is nested in.
    parent: Option<usize>,
}

/// The symbol tree as the Outline pane reads it: one row per symbol, depth first, which is the
/// order of the file.
fn flatten(symbols: &[Symbol]) -> Vec<Row> {
    fn walk(rows: &mut Vec<Row>, symbols: &[Symbol], depth: u8, parent: Option<usize>) {
        for symbol in symbols {
            let this = rows.len();
            rows.push(Row {
                depth,
                name: symbol.name.clone(),
                at: symbol.selection.start,
                lines: (symbol.range.start.line, symbol.range.end.line),
                parent,
            });
            walk(rows, &symbol.children, depth.saturating_add(1), Some(this));
        }
    }
    let mut rows = Vec::new();
    walk(&mut rows, symbols, 1, None);
    rows
}

/// The row of the innermost symbol whose range holds `line`, or `None` where no symbol does.
///
/// A search rather than a walk, since it runs on every caret move: the rows are in the order of
/// the file, so the last one to start at or before `line` is found by bisection. When that one
/// has already ended — a caret after a function's closing brace — the answer is one of the
/// symbols around it, because nothing that ended before it started can still hold the line.
fn row_at(rows: &[Row], line: u32) -> Option<usize> {
    let mut row = rows.partition_point(|r| r.lines.0 <= line).checked_sub(1)?;
    while rows[row].lines.1 < line {
        row = rows[row].parent?;
    }
    Some(row)
}

/// The line to pin above the view when `top` is the first line on screen: the line naming the
/// deepest symbol whose body the reader is inside and whose own head has scrolled away.
///
/// The *selection* line rather than the range's first, because a server includes a symbol's doc
/// comment in its range: what a reader has lost sight of is the signature, not the paragraph
/// above it. `None` when nothing has been scrolled out of sight, which takes the bar down again.
pub fn innermost(symbols: &[Symbol], top: u32) -> Option<u32> {
    symbols.iter().find_map(|symbol| {
        let head = symbol.selection.start.line;
        match head < top && top <= symbol.range.end.line {
            // Deepest first: a method inside a class is the better answer of the two.
            true => innermost(&symbol.children, top).or(Some(head)),
            false => None,
        }
    })
}

/// The icon for a completion kind. The names are the app's own, shipped in the GResource
/// (`data/icons/scalable/actions`) because Adwaita has no glyph for a function, an enum member or
/// a type parameter. Kinds that mean the same thing to a reader share one drawing: a constructor
/// is a method, a property is a field.
pub fn icon_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Text => "lsp-text-symbolic",
        Kind::Method | Kind::Constructor => "lsp-method-symbolic",
        Kind::Function => "lsp-function-symbolic",
        Kind::Field | Kind::Property => "lsp-field-symbolic",
        Kind::Variable => "lsp-variable-symbolic",
        Kind::Class => "lsp-class-symbolic",
        Kind::Interface => "lsp-interface-symbolic",
        Kind::Module => "lsp-module-symbolic",
        Kind::Enum => "lsp-enum-symbolic",
        Kind::EnumMember => "lsp-enum-member-symbolic",
        Kind::Keyword => "lsp-keyword-symbolic",
        Kind::Snippet => "lsp-snippet-symbolic",
        Kind::Constant => "lsp-constant-symbolic",
        Kind::Struct => "lsp-struct-symbolic",
        Kind::TypeParameter => "lsp-type-parameter-symbolic",
        Kind::File => "lsp-file-symbolic",
        Kind::Folder => "lsp-folder-symbolic",
        Kind::Tag => "lsp-tag-symbolic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use accent_api::Range;

    fn symbol(name: &str, line: u32, children: Vec<Symbol>) -> Symbol {
        let at = Range {
            start: Pos { line, character: 0 },
            end: Pos { line, character: 0 },
        };
        Symbol {
            name: name.to_string(),
            range: at,
            selection: at,
            children,
        }
    }

    /// The deepest symbol wins, and a reader who can still see a symbol's first line is told
    /// nothing about it.
    #[test]
    fn the_sticky_line_is_the_innermost_block_whose_head_is_off_screen() {
        let mut outer = symbol("Class", 0, vec![]);
        outer.range.end.line = 40;
        let mut inner = symbol("method", 10, vec![]);
        inner.range.end.line = 20;
        outer.children = vec![inner];
        let tree = [outer];
        assert_eq!(innermost(&tree, 15), Some(10));
        assert_eq!(innermost(&tree, 30), Some(0));
        assert_eq!(innermost(&tree, 0), None);
        assert_eq!(innermost(&tree, 41), None);
    }

    /// Depth first, and a child is one step deeper than its parent: the pane indents from that
    /// number and reads top to bottom, so the order is the order of the file.
    #[test]
    fn flatten_walks_the_tree_in_reading_order() {
        let tree = vec![
            symbol(
                "Title",
                0,
                vec![symbol("Section", 4, vec![symbol("Deeper", 8, vec![])])],
            ),
            symbol("Next", 12, vec![]),
        ];
        let rows: Vec<(u8, String)> = flatten(&tree)
            .into_iter()
            .map(|row| (row.depth, row.name))
            .collect();
        assert_eq!(
            rows,
            [
                (1, "Title".to_string()),
                (2, "Section".to_string()),
                (3, "Deeper".to_string()),
                (1, "Next".to_string()),
            ]
        );
    }

    /// The deepest row whose range holds the line, and no row at all where none does: above the
    /// first heading, or between two functions with nothing around them.
    #[test]
    fn the_caret_is_in_the_innermost_symbol_holding_its_line() {
        let span = |name: &str, first: u32, last: u32, children: Vec<Symbol>| {
            let mut s = symbol(name, first, children);
            s.range.end.line = last;
            s
        };
        // A note: sections run on to the next heading that is not below them.
        let note = flatten(&[span(
            "Title",
            2,
            30,
            vec![
                span("A", 5, 19, vec![span("A.1", 9, 19, vec![])]),
                span("B", 20, 30, vec![]),
            ],
        )]);
        fn at(rows: &[Row], line: u32) -> Option<&str> {
            row_at(rows, line).map(|row| rows[row].name.as_str())
        }
        assert_eq!(at(&note, 0), None, "above the first heading");
        assert_eq!(at(&note, 2), Some("Title"), "on the heading itself");
        assert_eq!(at(&note, 7), Some("A"));
        assert_eq!(at(&note, 12), Some("A.1"));
        assert_eq!(at(&note, 20), Some("B"));
        assert_eq!(at(&note, 30), Some("B"), "the last line");
        // Code: a method is inside its impl, and a gap has only what encloses it.
        let code = flatten(&[
            span(
                "Impl",
                0,
                20,
                vec![span("one", 2, 5, vec![]), span("two", 8, 12, vec![])],
            ),
            span("main", 25, 30, vec![]),
        ]);
        assert_eq!(at(&code, 3), Some("one"));
        assert_eq!(at(&code, 15), Some("Impl"), "after its last method");
        assert_eq!(at(&code, 22), None, "between two items");
        assert_eq!(at(&code, 40), None, "past the last one");
    }

    /// Above the first symbol is the one place without a row that sends the list to its top: a
    /// gap between two functions keeps it where it is.
    #[test]
    fn only_a_caret_before_every_symbol_is_above_the_outline() {
        let state = State::default();
        let mut first = symbol("first", 3, vec![]);
        first.range.end.line = 5;
        *state.rows.borrow_mut() = flatten(&[first, symbol("second", 9, vec![])]);
        assert!(state.above_outline(0));
        assert!(!state.above_outline(3), "on the first one");
        assert!(!state.above_outline(7), "between the two");
    }

    /// Every kind names a file that is actually in the GResource directory: a missing icon is a
    /// blank cell in the completion list and nothing on the console.
    #[test]
    fn every_kind_has_a_shipped_icon() {
        let kinds = [
            Kind::Text,
            Kind::Method,
            Kind::Function,
            Kind::Constructor,
            Kind::Field,
            Kind::Variable,
            Kind::Class,
            Kind::Interface,
            Kind::Module,
            Kind::Property,
            Kind::Enum,
            Kind::Keyword,
            Kind::Snippet,
            Kind::File,
            Kind::Folder,
            Kind::EnumMember,
            Kind::Constant,
            Kind::Struct,
            Kind::TypeParameter,
            Kind::Tag,
        ];
        for kind in kinds {
            let path = format!(
                "{}/data/icons/scalable/actions/{}.svg",
                env!("CARGO_MANIFEST_DIR"),
                icon_name(kind)
            );
            assert!(std::path::Path::new(&path).exists(), "{path}");
        }
    }
}
