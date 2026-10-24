//! Keep each tab's language-server document and providers in sync with its buffer.
//!
//! [`attach`] opens and wires the document; [`changed`] updates it and refreshes invalidated
//! answers; [`detach`] closes it. Requests run as local futures, and dropping one cancels it
//! at the server. Coalescing edits lets the latest win with one round trip per typing burst.

use crate::completion::{Reply, Session};
use crate::editor::{Flavour, Tab};
use accent_api::debug::Evaluated;
use accent_api::{
    CodeSymbol, Diagnostic, Kind, Pos, Range, Support, Symbol, SymbolKind, Task, Vault,
};
use futures_channel::oneshot;
use gtk::prelude::*;
use gtk::{gdk, glib};
use sourceview5::prelude::*;
use std::cell::{Cell, Ref, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// Post-edit symbol/fold debounce for Outline, sticky headers and chevrons, matching preview timing.
/// Ghost text uses shorter [`GHOST`] delay; the remaining delay follows it.
pub(crate) const REFRESH: Duration = Duration::from_millis(300);

/// Short ghost-text debounce so caret-local suggestions do not appear to disappear while typing.
const GHOST: Duration = Duration::from_millis(100);

/// Server-start delay before showing status, suppressing brief handshakes while reporting slow cold starts.
const SLOW_START: Duration = Duration::from_secs(1);

/// A callback the window registered, in the shape `editor.rs` uses for its own.
pub type Hook = Rc<dyn Fn(&Rc<Tab>)>;

/// The window's evaluation of an expression in its debugged program.
pub type Evaluate = Rc<dyn Fn(&Rc<Tab>, String) -> Option<accent_api::Task<Evaluated>>>;

/// What the language layer hands back to the window when an answer arrives.
pub struct Hooks {
    /// New symbols for this tab: the Outline pane and the sticky title are drawn from them.
    pub on_symbols: Hook,
    /// What the provider can do is known, or known again: the Code pane shows for one that
    /// answers goals.
    pub on_support: Hook,
    /// The document has waited [`SLOW_START`] on its server, or no longer waits: see
    /// [`Document::starting`].
    pub on_starting: Hook,
    /// An expression's value in the debugged program, for the hover: `None` unless it is paused
    /// and this is a file its debugger runs.
    pub evaluate: Evaluate,
}

/// A document open on the vault's language layer, kept in step with the text it stands for: a
/// code tab's file, or a notebook's code cells as one program.
/// Its owner pairs opening and closing, so two tabs of one path share no entries.
#[derive(Default)]
pub struct Document {
    /// What it is open as: its file's key (vault-relative, or absolute outside the vault), or a
    /// path no file stands behind.
    key: RefCell<String>,
    /// The vault it is open on. `None` before it is opened, and for a file outside every vault,
    /// which has nobody to ask.
    vault: RefCell<Option<Arc<Vault>>>,
    /// Shared provider capabilities, `None` until open.
    /// Avoid copying trigger vectors and strings for every typed character.
    support: RefCell<Option<Rc<Support>>>,
    /// Edit version and last sent version.
    /// Versions distinguish an in-flight flush from text the server actually received.
    version: Cell<u64>,
    sent: Cell<u64>,
    /// The callers waiting out the flush in flight, `None` while there is none: the next caller
    /// waits for it rather than starting a second one.
    waiting: RefCell<Option<Vec<oneshot::Sender<()>>>>,
    /// The "no language server" toast has been said for this document; it is not said again.
    toasted: Cell<bool>,
    /// The language server the document waits on to open, by name, once that has taken
    /// [`SLOW_START`]; `None` before and once it answers or fails.
    starting: RefCell<Option<String>>,
}

impl Document {
    pub fn key(&self) -> String {
        self.key.borrow().clone()
    }

    /// The vault to ask, once the document is open on one.
    pub fn vault(&self) -> Option<Arc<Vault>> {
        self.vault.borrow().clone()
    }

    /// What the provider can do; `None` while the document is still opening.
    pub fn support(&self) -> Option<Rc<Support>> {
        self.support.borrow().clone()
    }

    pub fn starting(&self) -> Option<String> {
        self.starting.borrow().clone()
    }

    /// Whether the "no language server" toast still has to be said, marking it said.
    pub fn claim_toast(&self) -> bool {
        !self.toasted.replace(true)
    }

    /// The text changed: the server's copy is stale until the next [`flush`](Self::flush).
    pub fn edited(&self) {
        self.version.set(self.version.get().wrapping_add(1));
    }

    /// How many times the text has changed, to tell later whether it has since.
    pub fn version(&self) -> u64 {
        self.version.get()
    }

    /// Whether the server holds the text as it is: every change sent.
    pub fn in_step(&self) -> bool {
        self.sent.get() >= self.version.get()
    }

    /// Open as `key` in `language` (a GtkSourceView id) holding `text`, the key and the vault
    /// taken at once, so what is asked meanwhile goes there. `told` is called when
    /// [`starting`](Self::starting) changes: once the server has kept it waiting [`SLOW_START`]
    /// (DESIGN.md, Status bar), and once it answers or fails. The support is kept when it opens.
    pub fn open<F: Fn() + 'static>(
        self: &Rc<Self>,
        vault: Arc<Vault>,
        key: &str,
        language: &str,
        text: String,
        told: F,
    ) -> impl Future<Output = accent_api::Result<Support>> + use<F> {
        *self.vault.borrow_mut() = Some(vault.clone());
        self.key.replace(key.to_string());
        let (me, key, language) = (Rc::downgrade(self), key.to_string(), language.to_string());
        async move {
            let told = Rc::new(told);
            let saying = vault.server_name(&language).map(|server| {
                let (me, told) = (me.clone(), told.clone());
                glib::spawn_future_local(async move {
                    glib::timeout_future(SLOW_START).await;
                    if let Some(me) = me.upgrade() {
                        me.set_starting(Some(server), &*told);
                    }
                })
            });
            let opened = vault.open_document(&key, &language, text).await;
            if let Some(saying) = saying {
                saying.abort();
            }
            if let Some(me) = me.upgrade() {
                me.set_starting(None, &*told);
                if let Ok(support) = &opened {
                    *me.support.borrow_mut() = Some(Rc::new(support.clone()));
                }
            }
            opened
        }
    }

    /// Say that the document waits on `server`, or does not, calling `told` if that changed.
    fn set_starting(&self, server: Option<String>, told: &dyn Fn()) {
        if *self.starting.borrow() == server {
            return;
        }
        self.starting.replace(server);
        told();
    }

    /// Send the edits made since the last flush, `text` being what it holds now, before a
    /// positional request so that the answer refers to it. `text` answers `None` when the
    /// document holds what was sent last whatever the edits were.
    ///
    /// Concurrent callers share one round trip. Failed changes leave the version unsent for
    /// the next caller to retry, and answer `false`: what `text` answered is not the server's.
    pub async fn flush(self: Rc<Self>, text: impl FnOnce() -> Option<String>) -> bool {
        // Looked at again once woken: another waiter may have started the next flush by then.
        loop {
            let woken = match self.waiting.borrow_mut().as_mut() {
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
        let Some(vault) = self.vault() else {
            return true;
        };
        let wanted = self.version.get();
        if self.sent.get() >= wanted {
            return true;
        }
        let (key, Some(text)) = (self.key(), text()) else {
            self.sent.set(wanted);
            return true;
        };
        tracing::debug!("changed {key}, {} chars", text.chars().count());
        let flushing = Flushing::new(self.clone());
        let sent = vault.change_document(&key, text).await;
        drop(flushing);
        match sent {
            Ok(()) => {
                self.sent.set(self.sent.get().max(wanted));
                true
            }
            Err(e) => {
                tracing::debug!("changing {key}: {e:#}");
                false
            }
        }
    }

    /// Close it on its vault, and cancel what was asked of it there: what its owner's drop does.
    pub fn close(&self) {
        let Some(vault) = self.vault.borrow_mut().take() else {
            return;
        };
        let key = self.key();
        glib::spawn_future_local(async move {
            tracing::debug!("closing {key}");
            if let Err(e) = vault.close_document(&key).await {
                tracing::debug!("closing {key}: {e:#}");
            }
        });
    }
}

/// Tab-owned language state: its document, and what the tab draws from the answers.
#[derive(Default)]
pub struct State {
    /// The tab's file on the language layer.
    pub doc: Rc<Document>,
    /// The document's symbols, most recent answer.
    symbols: RefCell<Vec<Symbol>>,
    /// Flattened symbol rows cached once per answer, avoiding repeated work on caret moves.
    rows: RefCell<Vec<Row>>,
    /// The index's declarations of a code file, which stand in for the symbols while no language
    /// server has answered with any: see [`index_outline`].
    index: RefCell<Vec<Symbol>>,
    /// The last answer of a language server held symbols, which the outline is drawn from.
    served: Cell<bool>,
    /// The pending post-edit refresh. Replaced rather than queued, so the latest edit wins.
    refresh: RefCell<Option<glib::JoinHandle<()>>>,
    hooks: RefCell<Option<Rc<Hooks>>>,
    /// The signature popover of this tab, and the request that would fill it.
    pub signature: crate::signature::Help,
    /// The completion popup of this tab, once [`attach`] gave it one.
    pub(crate) completion: RefCell<Option<Rc<crate::completion::Session>>>,
    /// Whether ghost text is wanted here and whether anything is standing in its way.
    pub ghost: crate::ghost::State,
    /// The line the server says it is still checking from (`Event::Checking`), `None` once it
    /// is done and for every server that does not say.
    pub checking: Cell<Option<u32>>,
}

impl State {
    /// The vault to ask, for a tab that has one.
    pub fn vault(&self) -> Option<Arc<Vault>> {
        self.doc.vault()
    }

    pub fn hooks(&self) -> Option<Rc<Hooks>> {
        self.hooks.borrow().clone()
    }

    /// What the provider can do; `None` while the document is still opening.
    pub fn support(&self) -> Option<Rc<Support>> {
        self.doc.support()
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

    /// Whether the outline is a language server's rather than the index's.
    #[cfg(feature = "bench")]
    pub fn served(&self) -> bool {
        self.served.get()
    }

    /// Whether the "no language server" toast still has to be said, marking it said.
    pub fn claim_toast(&self) -> bool {
        self.doc.claim_toast()
    }

    /// Whether the document is a dependency outside the vault, answered by the server that
    /// named it: see [`Support::dependency`].
    pub fn is_dependency(&self) -> bool {
        self.support().is_some_and(|s| s.dependency)
    }
}

/// What the language features (hover, signature help, completion and the key chain) ask of the
/// editor they work in: a code tab's view, over its whole document, or a notebook code cell's,
/// over that cell's lines of the notebook's.
pub(crate) trait Editor {
    fn view(&self) -> &sourceview5::View;
    fn document(&self) -> Rc<Document>;
    /// Send the document what has been typed, before a position in it is asked about.
    fn flush(self: Rc<Self>) -> Reply<()>;
    /// Where `at` in the view is in the document, read after a [`flush`](Self::flush); `None` for
    /// a view that is no part of it any more.
    fn to_document(&self, at: Pos) -> Option<Pos>;
    /// Where document position `at` is in the view: [`to_document`](Self::to_document) backwards,
    /// `None` outside the view's part of the document.
    fn to_view(&self, at: Pos) -> Option<Pos> {
        Some(at)
    }
    /// What the server said about the view's text, in the view's positions.
    fn diagnostics(&self) -> Ref<'_, Vec<Diagnostic>>;
    /// The LSP language id: see [`language_id`].
    fn language_id(&self) -> String;
    fn flavour(&self) -> Flavour;
    /// Whether the text is being put in rather than typed, which opens nothing.
    fn is_loading(&self) -> bool;
    fn signature(&self) -> &crate::signature::Help;
    fn completion(&self) -> Option<Rc<Session>>;
    /// Whether a template's or a completion's stops are being walked.
    fn snippet_active(&self) -> bool;
    /// Stop all active snippets by toggling `enable-snippets`, the available GtkSourceView 5.20
    /// cancellation mechanism.
    fn end_snippet(&self) {
        self.view().set_enable_snippets(false);
        self.view().set_enable_snippets(true);
    }
    /// The paused program's value of the name at `start`, as it is asked for: the hover's first
    /// line.
    fn value(self: Rc<Self>, _start: &gtk::TextIter) -> Option<(String, Task<Evaluated>)> {
        None
    }
    /// A key for the ghost text painted, if any: see [`crate::ghost::on_key`].
    fn ghost_key(
        self: Rc<Self>,
        _key: gdk::Key,
        _state: gdk::ModifierType,
    ) -> Option<glib::Propagation> {
        None
    }
    /// Whether the server's quick fixes are offered here: wherever the text can be edited.
    fn offers_fixes(&self) -> bool {
        self.view().is_editable()
    }
}

impl Editor for Tab {
    fn view(&self) -> &sourceview5::View {
        &self.view
    }

    fn document(&self) -> Rc<Document> {
        self.lang.doc.clone()
    }

    fn flush(self: Rc<Self>) -> Reply<()> {
        Box::pin(flush(self))
    }

    fn to_document(&self, at: Pos) -> Option<Pos> {
        Some(at)
    }

    fn diagnostics(&self) -> Ref<'_, Vec<Diagnostic>> {
        Tab::diagnostics(self)
    }

    fn language_id(&self) -> String {
        language_id(self)
    }

    fn flavour(&self) -> Flavour {
        Tab::flavour(self)
    }

    fn is_loading(&self) -> bool {
        Tab::is_loading(self)
    }

    fn signature(&self) -> &crate::signature::Help {
        &self.lang.signature
    }

    fn completion(&self) -> Option<Rc<Session>> {
        crate::completion::session(self)
    }

    fn snippet_active(&self) -> bool {
        Tab::snippet_active(self)
    }

    fn value(self: Rc<Self>, start: &gtk::TextIter) -> Option<(String, Task<Evaluated>)> {
        crate::hover::value_of(&self, start)
    }

    fn ghost_key(
        self: Rc<Self>,
        key: gdk::Key,
        state: gdk::ModifierType,
    ) -> Option<glib::Propagation> {
        crate::ghost::on_key(&self, key, state)
    }
}

/// Request position as zero-based line and character column, matching TextIter coordinates.
pub fn pos_of(iter: &gtk::TextIter) -> Pos {
    Pos {
        line: iter.line().max(0) as u32,
        character: iter.line_offset().max(0) as u32,
    }
}

/// Convert `pos` to a buffer iter, clamping stale server positions to avoid GTK criticals.
pub fn iter_at(buffer: &impl IsA<gtk::TextBuffer>, pos: Pos) -> gtk::TextIter {
    let end = crate::editor::line_end(buffer, pos.line as i32);
    let mut iter = end;
    iter.set_line_offset((pos.character as i32).clamp(0, end.line_offset()));
    iter
}

/// LSP language ID from GtkSourceView, or markdown for locally styled notes.
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

/// Open the tab's language document and wire its providers.
///
/// Skip CSV because styling is local and language queries add no results. Without a vault,
/// notes and org files use [`outline_alone`] and other files have no language-layer providers.
pub fn attach(tab: &Rc<Tab>, vault: Option<Arc<Vault>>, hooks: Hooks) {
    if matches!(tab.flavour(), Flavour::Csv(_)) {
        return;
    }
    *tab.lang.hooks.borrow_mut() = Some(Rc::new(hooks));
    let Some(vault) = vault else {
        return restart(tab, Duration::ZERO);
    };
    let editor: Rc<dyn Editor> = tab.clone();
    let source = crate::completion::TabSource(Rc::downgrade(tab));
    let session = crate::completion::install(&editor, Box::new(source));
    *tab.lang.completion.borrow_mut() = Some(session);
    crate::hover::install(&editor);
    crate::signature::install(&editor);
    crate::ghost::install(tab);
    join(tab, vault);
}

/// Open the tab's document on `vault`, unless it is open there already or opening. A file from
/// outside the vault opens only as a dependency a running server named ([`Support::dependency`]);
/// any other keeps no vault, as a tab outside every vault has none, until it is joined again.
pub fn join(tab: &Rc<Tab>, vault: Arc<Vault>) {
    if tab.lang.vault().is_some() {
        return;
    }
    let opening = open(tab, vault);
    index_outline(tab);

    let (rel, id) = (tab.language_key(), language_id(tab));
    let weak = Rc::downgrade(tab);
    glib::spawn_future_local(async move {
        let support = opening.await;
        let Some(tab) = weak.upgrade() else { return };
        match support {
            Ok(support) if crate::doc::is_loose_key(&rel) && !support.dependency => {
                tab.lang.doc.vault.take();
                tab.lang.doc.support.take();
            }
            Ok(support) => {
                tracing::debug!("opened {rel} as {id}: {support:?}");
                supported(&tab);
            }
            Err(e) => tracing::warn!("cannot open {rel} on the language layer: {e:#}"),
        }
    });
}

/// Mark every edit unsent before debounced [`changed`] so immediate positional requests flush current text.
pub fn edited(tab: &Tab) {
    tab.lang.doc.edited();
}

/// The buffer changed: the server's copy is stale and everything derived from it is too.
pub fn changed(tab: &Rc<Tab>) {
    edited(tab);
    // A signature that is up is about the call being typed, so it is asked again rather than
    // left saying what the last keystroke meant.
    if tab.lang.signature.is_shown() {
        crate::signature::request(&(tab.clone() as Rc<dyn Editor>));
    }
    restart(tab, REFRESH);
}

/// Notify the server of save after flushing edits, enabling save-triggered compiler diagnostics.
pub fn saved(tab: &Rc<Tab>) {
    let Some(vault) = tab.lang.vault() else {
        return;
    };
    index_outline(tab);
    let tab = tab.clone();
    glib::spawn_future_local(async move {
        flush(tab.clone()).await;
        let rel = tab.language_key();
        if let Err(e) = vault.save_document(&rel).await {
            tracing::debug!("saved {rel}: {e:#}");
        }
    });
}

/// Fetch indexed declarations as fallback Outline symbols before a server answers.
/// The index describes saved text, so refresh on opening, saving and index changes, not each keystroke.
pub fn index_outline(tab: &Rc<Tab>) {
    let rel = tab.rel();
    if tab.flavour() != Flavour::Code
        || tab.lang.served.get()
        || accent_core::code::lang_of(&rel).is_none()
        || crate::doc::is_loose_key(&rel)
    {
        return;
    }
    let Some(vault) = tab.lang.vault() else {
        return;
    };
    let text = tab.text();
    let weak = Rc::downgrade(tab);
    glib::spawn_future_local(async move {
        let found = crate::work::off_thread("outline", move || {
            // A save is taken in by the worker a batch later.
            vault.settle_index();
            vault
                .file_symbols(&rel)
                .map(|decls| declarations(&rel, &decls, &text))
        })
        .await;
        let Some(tab) = weak.upgrade() else { return };
        let symbols = match found {
            Some(Ok(symbols)) => symbols,
            Some(Err(e)) => return tracing::debug!("declarations of {}: {e:#}", tab.rel()),
            None => return,
        };
        *tab.lang.index.borrow_mut() = symbols.clone();
        if !tab.lang.served.get() {
            show_symbols(&tab, symbols);
        }
    });
}

/// Notify the provider on leaving the document for expensive save-related work.
/// Vault-wide ghost-index refresh belongs here rather than every autosave.
pub fn settle(tab: &Rc<Tab>) {
    let Some(vault) = tab.lang.vault() else {
        return;
    };
    let tab = tab.clone();
    glib::spawn_future_local(async move {
        flush(tab.clone()).await;
        let rel = tab.language_key();
        if let Err(e) = vault.settle(&rel).await {
            tracing::debug!("settling {rel}: {e:#}");
        }
    });
}

/// Refresh hints after index changes, including newly resolvable wikilinks without edits to this note.
pub fn rediagnose(tab: &Rc<Tab>) {
    let Some(vault) = tab.lang.vault() else {
        return;
    };
    let rel = tab.language_key();
    glib::spawn_future_local(async move {
        if let Err(e) = vault.rediagnose(&rel).await {
            tracing::debug!("re-diagnosing {rel}: {e:#}");
        }
    });
}

/// Close the language document and cancel requests from Tab::drop, retaining only vault and path in the future.
pub fn detach(tab: &Tab) {
    tab.lang.signature.dismiss();
    if let Some(session) = tab.lang.completion.take() {
        session.close();
    }
    if let Some(handle) = tab.lang.refresh.borrow_mut().take() {
        handle.abort();
    }
    tab.lang.doc.close();
}

/// A rename landed: the old path is closed and the new one opened, because a language server
/// keys its documents by URI and knows nothing of the move.
pub fn retarget(tab: &Rc<Tab>, old_rel: &str) {
    let Some(vault) = tab.lang.vault() else {
        return;
    };
    let old = old_rel.to_string();
    let opening = open(tab, vault.clone());
    let weak = Rc::downgrade(tab);
    glib::spawn_future_local(async move {
        let _ = vault.close_document(&old).await;
        let support = opening.await;
        let Some(tab) = weak.upgrade() else { return };
        if support.is_ok() {
            supported(&tab);
        }
    });
}

/// Open the tab's document on `vault` as it stands, the window told which server it waits on.
fn open(
    tab: &Rc<Tab>,
    vault: Arc<Vault>,
) -> impl Future<Output = accent_api::Result<Support>> + use<> {
    let weak = Rc::downgrade(tab);
    let told = move || {
        let tab = weak.upgrade();
        if let Some((tab, hooks)) = tab.and_then(|tab| Some((tab.clone(), tab.lang.hooks()?))) {
            (hooks.on_starting)(&tab);
        }
    };
    let doc = &tab.lang.doc;
    doc.open(
        vault,
        &tab.language_key(),
        &language_id(tab),
        tab.text(),
        told,
    )
}

/// The provider said what it can do: tell the window, and read what it answers.
fn supported(tab: &Rc<Tab>) {
    let hooks = tab.lang.hooks.borrow().clone();
    if let Some(hooks) = hooks {
        (hooks.on_support)(tab);
    }
    restart(tab, Duration::ZERO);
}

/// Close and reopen with current text for Lean Restart File, rereading imports.
/// Closing the last document also shuts down its idle server session.
pub fn reopen(tab: &Rc<Tab>) {
    retarget(tab, &tab.rel());
}

/// Flush newer edits and refresh answers after server replacement.
/// Reconnect may have restored older text than edits refused during the connection attempt.
pub fn resync(tab: &Rc<Tab>) {
    restart(tab, Duration::ZERO);
}

/// Flush pending edits before positional requests so answers refer to current text: see
/// [`Document::flush`].
pub async fn flush(tab: Rc<Tab>) {
    tab.lang.doc.clone().flush(|| Some(tab.text())).await;
}

/// RAII in-flight flush marker that wakes waiters on completion or cancellation.
/// Dropped refresh futures must release it so later requests cannot wait forever.
struct Flushing(Rc<Document>);

impl Flushing {
    fn new(doc: Rc<Document>) -> Self {
        *doc.waiting.borrow_mut() = Some(Vec::new());
        Flushing(doc)
    }
}

impl Drop for Flushing {
    fn drop(&mut self) {
        for wake in self.0.waiting.take().into_iter().flatten() {
            // A waiter dropped meanwhile is not there to hear it, and needs nothing.
            let _ = wake.send(());
        }
    }
}

/// Replace the pending post-edit future, cancelling old server requests so bursts retain one active refresh.
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

/// Wait only for nonzero delays; a zero timeout still adds an unwanted main-loop turn.
async fn pause(delay: Duration) {
    if !delay.is_zero() {
        glib::timeout_future(delay).await;
    }
}

/// Flush edits then refresh symbols and folds in one cancellable future.
/// `rest` is the remaining delay after ghost completion.
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
    let rel = tab.language_key();
    match vault.symbols(&rel).await {
        Ok(symbols) if !symbols.is_empty() => {
            tab.lang.served.set(true);
            set_symbols(&tab, symbols);
        }
        // No symbols from a server, or no server at all: the index's declarations stand in. A
        // request that failed keeps what a server last gave.
        answer => {
            if let Err(e) = &answer {
                tracing::debug!("symbols for {rel}: {e:#}");
            }
            if answer.is_ok() || !tab.lang.served.get() {
                tab.lang.served.set(false);
                let index = tab.lang.index.borrow().clone();
                set_symbols(&tab, index);
            }
        }
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

/// [`refresh`] for a note or an org file outside every vault: its headings and folds are read
/// from its own text, off the main loop, as the notes provider reads them inside one.
async fn outline_alone(tab: Rc<Tab>, rest: Duration) {
    if !tab.flavour().is_note() && !tab.is_org() {
        return;
    }
    pause(rest).await;
    let (edits, text, rel) = (tab.save.edits.get(), tab.text(), tab.rel());
    let outline = crate::work::off_thread("outline", move || {
        accent_api::language::note_outline(&rel, &text)
    });
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

fn set_symbols(tab: &Tab, symbols: Vec<Symbol>) {
    *tab.lang.rows.borrow_mut() = flatten(&symbols);
    *tab.lang.symbols.borrow_mut() = symbols;
}

/// [`set_symbols`], and the panes drawn from them told.
fn show_symbols(tab: &Rc<Tab>, symbols: Vec<Symbol>) {
    set_symbols(tab, symbols);
    let hooks = tab.lang.hooks.borrow().clone();
    if let Some(hooks) = hooks {
        (hooks.on_symbols)(tab);
    }
}

/// Convert index declarations into nested symbols positioned in `text`.
/// Span containment creates children; out-of-type method runs receive synthetic type rows.
fn declarations(rel: &str, decls: &[CodeSymbol], text: &str) -> Vec<Symbol> {
    let lines: Vec<&str> = text.lines().collect();
    let rust = accent_core::code::lang_of(rel) == Some(accent_core::code::Lang::Rust);
    let (mut at, mut out) = (0, Vec::<Symbol>::new());
    // The type whose methods the last row gathers, while it is such a row.
    let mut gathering = None;
    while let Some(d) = decls.get(at) {
        let symbol = declaration(decls, &mut at, &lines);
        match d.container.as_deref() {
            None => {
                gathering = None;
                out.push(symbol);
            }
            Some(owner) if gathering == Some(owner) => {
                if let Some(group) = out.last_mut() {
                    group.range.end = symbol.range.end;
                    group.children.push(symbol);
                }
            }
            Some(owner) => {
                gathering = Some(owner);
                out.push(Symbol {
                    name: match rust {
                        true => format!("impl {owner}"),
                        false => owner.to_string(),
                    },
                    range: symbol.range,
                    selection: symbol.selection,
                    children: vec![symbol],
                });
            }
        }
    }
    out
}

/// The declaration at `at` with every one inside its span, `at` moved past them all.
fn declaration(decls: &[CodeSymbol], at: &mut usize, lines: &[&str]) -> Symbol {
    let d = &decls[*at];
    *at += 1;
    let mut children = Vec::new();
    while decls
        .get(*at)
        .is_some_and(|c| c.byte_start < d.byte_end && c.byte_end <= d.byte_end)
    {
        children.push(declaration(decls, at, lines));
    }
    let first = d.line.saturating_sub(1);
    let last = d.end_line.saturating_sub(1).max(first);
    let name = name_at(lines, first, last, &d.name);
    let line = |line| Pos { line, character: 0 };
    Symbol {
        name: d.name.clone(),
        range: Range {
            start: line(first),
            end: line(last),
        },
        selection: Range {
            start: name,
            end: name,
        },
        children,
    }
}

/// Find `name` as a word in `first..last` lines (`accent_core::code::name_at`, a line at a time),
/// falling back to the start of `first`. Declaration jumps should target names rather than
/// attributes.
pub fn name_at(lines: &[&str], first: u32, last: u32, name: &str) -> Pos {
    let span = lines.iter().enumerate().skip(first as usize);
    for (line, text) in span.take(last.saturating_sub(first) as usize + 1) {
        if let Some(i) = accent_core::code::name_at(text, 0..text.len(), name) {
            return Pos {
                line: line as u32,
                character: text[..i].chars().count() as u32,
            };
        }
    }
    Pos {
        line: first,
        character: 0,
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

/// Find the innermost symbol row containing `line`, or `None`.
///
/// Bisection finds the last symbol starting before the caret; if it ended already, enclosing
/// symbols are searched. This avoids a full walk on every caret move.
fn row_at(rows: &[Row], line: u32) -> Option<usize> {
    let mut row = rows.partition_point(|r| r.lines.0 <= line).checked_sub(1)?;
    while rows[row].lines.1 < line {
        row = rows[row].parent?;
    }
    Some(row)
}

/// Find the deepest enclosing symbol whose header scrolled above `top`, or `None`.
///
/// Use its selection line, since server ranges may begin at doc comments and the pinned
/// header should show the signature.
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

/// Completion-kind icons shipped as GResources because Adwaita lacks semantic code glyphs.
/// Kinds with the same meaning share icons, such as constructor/method and property/field.
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

/// Map indexed declarations to the corresponding completion icon, matching server trait/type-alias meanings.
pub fn declaration_icon(kind: SymbolKind) -> &'static str {
    icon_name(match kind {
        SymbolKind::Function | SymbolKind::Macro => Kind::Function,
        SymbolKind::Method => Kind::Method,
        SymbolKind::Struct => Kind::Struct,
        SymbolKind::Enum => Kind::Enum,
        SymbolKind::Trait => Kind::Interface,
        SymbolKind::Class => Kind::Class,
        SymbolKind::TypeAlias => Kind::TypeParameter,
        SymbolKind::Module => Kind::Module,
        SymbolKind::Const => Kind::Constant,
    })
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

    /// The index's flat rows nest as a server's do: what one's span holds under it, and the
    /// methods of an `impl` under a row of their own, each landing on its name.
    #[test]
    fn index_declarations_nest_as_an_outline() {
        use accent_core::code::{Lang, symbols};
        let text = "pub struct Point;\n\nimpl Point {\n    #[inline]\n    pub fn x(&self) {}\n    \
                    fn y(&self) {}\n}\n\nfn main() {}\n\nmod tests {\n    fn t() {}\n}\n";
        let decls: Vec<CodeSymbol> = symbols(Lang::Rust, text)
            .into_iter()
            .map(|d| CodeSymbol {
                rel_path: "a.rs".to_string(),
                kind: d.kind,
                name: d.name,
                container: d.container,
                byte_start: d.range.start as i64,
                byte_end: d.range.end as i64,
                line: d.line,
                end_line: d.end_line,
                signature: d.signature,
                test: d.test,
            })
            .collect();
        let rows: Vec<(u8, String, u32, u32)> = flatten(&declarations("a.rs", &decls, text))
            .into_iter()
            .map(|r| (r.depth, r.name, r.at.line, r.at.character))
            .collect();
        let row = |depth, name: &str, line, column| (depth, name.to_string(), line, column);
        assert_eq!(
            rows,
            [
                row(1, "Point", 0, 11),
                row(1, "impl Point", 4, 11),
                row(2, "x", 4, 11),
                row(2, "y", 5, 7),
                row(1, "main", 8, 3),
                row(1, "tests", 10, 4),
                row(2, "t", 11, 7),
            ]
        );
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
