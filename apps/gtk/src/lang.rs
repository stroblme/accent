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
use gtk::glib;
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// DESIGN.md, Motion: the symbols and folds behind the Outline pane, the sticky title and the
/// gutter chevrons follow the last edit by 300 ms, as the preview does.
const REFRESH: Duration = Duration::from_millis(300);

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
    support: RefCell<Option<Support>>,
    /// The document's symbols, most recent answer.
    symbols: RefCell<Vec<Symbol>>,
    /// The pending post-edit refresh. Replaced rather than queued, so the latest edit wins.
    refresh: RefCell<Option<glib::JoinHandle<()>>>,
    /// The buffer has changed since the server was last told.
    dirty: Cell<bool>,
    /// The "no language server" toast has been said for this tab; it is not said again.
    toasted: Cell<bool>,
    /// The vault this document is open on. `None` for a tab outside every vault, which has
    /// nobody to ask and gets no providers.
    vault: RefCell<Option<Arc<Vault>>>,
    hooks: RefCell<Option<Rc<Hooks>>>,
    /// The signature popover of this tab, and the request that would fill it.
    pub signature: crate::signature::Help,
}

impl State {
    /// The vault to ask, for a tab that has one.
    pub fn vault(&self) -> Option<Arc<Vault>> {
        self.vault.borrow().clone()
    }

    /// What the provider can do; `None` while the document is still opening.
    pub fn support(&self) -> Option<Support> {
        self.support.borrow().clone()
    }

    pub fn symbols(&self) -> Vec<Symbol> {
        self.symbols.borrow().clone()
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

/// The LSP language id for a tab: the GtkSourceView language's own id, which is the same name
/// (`rust`, `c`, `python`), and `markdown` for a note, whose buffer carries no language because
/// our own styling pass does that job.
pub fn language_id(tab: &Tab) -> String {
    match tab.flavour() {
        Flavour::Note => "markdown".to_string(),
        _ => tab
            .buffer
            .language()
            .map(|l| l.id().to_string())
            .unwrap_or_default(),
    }
}

/// Open this tab's document on `vault` and wire the providers that read it.
///
/// A CSV is skipped: its columns are coloured by us and no language server speaks the format, so
/// opening it would only cost a round trip to be told nothing.
pub fn attach(tab: &Rc<Tab>, vault: Arc<Vault>, hooks: Hooks) {
    if tab.flavour() == Flavour::Csv {
        return;
    }
    *tab.lang.vault.borrow_mut() = Some(vault.clone());
    *tab.lang.hooks.borrow_mut() = Some(Rc::new(hooks));
    crate::completion::install(tab);
    crate::hover::install(tab);
    crate::signature::install(tab);

    let (rel, id, text) = (tab.rel(), language_id(tab), tab.text());
    let weak = Rc::downgrade(tab);
    glib::spawn_future_local(async move {
        let support = vault.open_document(&rel, &id, text).await;
        let Some(tab) = weak.upgrade() else { return };
        match support {
            Ok(support) => {
                tracing::debug!("opened {rel} as {id}: {support:?}");
                *tab.lang.support.borrow_mut() = Some(support);
                restart(&tab, Duration::ZERO);
            }
            Err(e) => tracing::warn!("cannot open {rel} on the language layer: {e:#}"),
        }
    });
}

/// The buffer changed: the server's copy is stale and everything derived from it is too.
pub fn changed(tab: &Rc<Tab>) {
    tab.lang.dirty.set(true);
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
            *tab.lang.support.borrow_mut() = Some(support);
            restart(&tab, Duration::ZERO);
        }
    });
}

/// Send the pending edit and wait for the server to have it. Every positional request awaits
/// this first: an answer about a text the server has not been given is an answer about the wrong
/// characters.
pub async fn flush(tab: Rc<Tab>) {
    let pending = match tab.lang.dirty.replace(false) {
        false => None,
        true => tab.lang.vault().map(|v| (v, tab.rel(), tab.text())),
    };
    let Some((vault, rel, text)) = pending else {
        return;
    };
    tracing::debug!("changed {rel}, {} chars", text.chars().count());
    if let Err(e) = vault.change_document(&rel, text).await {
        tracing::debug!("changing {rel}: {e:#}");
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
        if !delay.is_zero() {
            glib::timeout_future(delay).await;
        }
        let Some(tab) = weak.upgrade() else { return };
        refresh(tab).await;
    });
    *tab.lang.refresh.borrow_mut() = Some(handle);
}

/// Give the server the edit, then re-read what it implies: the symbols the Outline pane and the
/// sticky title are drawn from, and the blocks that can be folded.
async fn refresh(tab: Rc<Tab>) {
    let Some(vault) = tab.lang.vault() else {
        return;
    };
    flush(tab.clone()).await;
    let rel = tab.rel();
    match vault.symbols(&rel).await {
        Ok(symbols) => *tab.lang.symbols.borrow_mut() = symbols,
        Err(e) => tracing::debug!("symbols for {rel}: {e:#}"),
    }
    match vault.folds(&rel).await {
        Ok(folds) => tab.set_folds(folds),
        Err(e) => tracing::debug!("folds for {rel}: {e:#}"),
    }
    let hooks = tab.lang.hooks.borrow().clone();
    if let Some(hooks) = hooks {
        (hooks.on_symbols)(&tab);
    }
}

/// The symbol tree as the Outline pane reads it: one row per symbol, depth first, carrying how
/// deep it is (1 for a top-level one, which is what the pane indents from) and where a click on
/// it should land.
pub fn flatten(symbols: &[Symbol]) -> Vec<(u8, String, Pos)> {
    fn walk(rows: &mut Vec<(u8, String, Pos)>, symbols: &[Symbol], depth: u8) {
        for symbol in symbols {
            rows.push((depth, symbol.name.clone(), symbol.selection.start));
            walk(rows, &symbol.children, depth.saturating_add(1));
        }
    }
    let mut rows = Vec::new();
    walk(&mut rows, symbols, 1);
    rows
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
            .map(|(depth, name, _)| (depth, name))
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
