//! What a text tab asks about its own content: completion, hover, definition, symbols,
//! references, folds, and the diagnostics that arrive unasked.
//!
//! One shape for every text file. A code file is answered by a language server over
//! `accent-lsp`; a note is answered by the index (`notes.rs`). The UI never learns which. Every
//! position here is a line and a column in *characters* (Unicode scalars), which is what a
//! `GtkTextIter` counts in; the UTF-16 arithmetic a server wants happens next to the text, on
//! the host that holds it.
//!
//! The requests are futures on the shared runtime, wrapped in a [`Task`] that aborts when
//! dropped: a completion the user has typed past is cancelled at the server rather than answered
//! into the void.

// ponytail: the providers that use this land in the next commits.
#![allow(dead_code)]

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::Event;

/// A line and a column, both zero-based; the column counts characters from the line start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Pos {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Range {
    pub start: Pos,
    pub end: Pos,
}

/// Somewhere to go. `path` is vault-relative inside the vault, absolute outside it, and a
/// `scheme://` URL when the target is not a file at all (an external link in a note).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Location {
    pub path: String,
    pub range: Range,
}

impl Location {
    pub fn is_url(&self) -> bool {
        self.path.contains("://")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextEdit {
    pub range: Range,
    pub text: String,
}

/// What a completion item is, for the glyph beside it. The LSP kinds accent shows, plus `Tag`
/// for a note's `#tag`; anything else a server sends is `Text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Text,
    Method,
    Function,
    Constructor,
    Field,
    Variable,
    Class,
    Interface,
    Module,
    Property,
    Enum,
    Keyword,
    Snippet,
    File,
    Folder,
    EnumMember,
    Constant,
    Struct,
    TypeParameter,
    Tag,
}

impl Kind {
    /// The LSP `CompletionItemKind` number, or `Text` for one accent has no glyph for.
    pub fn from_lsp(n: u32) -> Kind {
        match n {
            2 => Kind::Method,
            3 => Kind::Function,
            4 => Kind::Constructor,
            5 => Kind::Field,
            6 => Kind::Variable,
            7 => Kind::Class,
            8 => Kind::Interface,
            9 => Kind::Module,
            10 => Kind::Property,
            13 => Kind::Enum,
            14 => Kind::Keyword,
            15 => Kind::Snippet,
            17 => Kind::File,
            19 => Kind::Folder,
            20 => Kind::EnumMember,
            21 => Kind::Constant,
            22 => Kind::Struct,
            25 => Kind::TypeParameter,
            _ => Kind::Text,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Completion {
    pub label: String,
    pub kind: Kind,
    /// A type or a path, shown after the label.
    pub detail: Option<String>,
    /// Markdown, shown in the details panel; may only arrive after `resolve`.
    pub doc: Option<String>,
    /// What typed text is matched against, when it is not the label.
    pub filter: Option<String>,
    pub insert: String,
    /// `insert` is a snippet (`$1`, `${2:name}`), not literal text.
    pub is_snippet: bool,
    /// What `insert` replaces: the word or trigger before the caret, and whatever the item wants
    /// eaten after it.
    pub replace: Range,
    /// Applied with the insert, in one undo step: an import, typically.
    pub extra_edits: Vec<TextEdit>,
    /// The item as the server sent it, when it can still be resolved for more; opaque to the UI.
    pub resolve: Option<serde_json::Value>,
}

/// The signature the caret is inside a call of.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signature {
    pub label: String,
    pub doc: Option<String>,
    /// Character ranges into `label`, one per parameter.
    pub params: Vec<(u32, u32)>,
    /// Which of `params` the caret is on.
    pub active: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hover {
    /// Markdown.
    pub text: String,
    pub range: Option<Range>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    /// The whole of it: the function body, the section under the heading.
    pub range: Range,
    /// The name alone, where a jump lands.
    pub selection: Range,
    pub children: Vec<Symbol>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub range: Range,
    pub severity: Severity,
    pub message: String,
    pub source: Option<String>,
}

/// Lines that can be hidden behind their first one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fold {
    pub start_line: u32,
    pub end_line: u32,
}

/// What attached to an opened document.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Support {
    /// Characters that open the completion popup as they are typed.
    pub completion_triggers: Vec<char>,
    /// Characters that ask for a signature as they are typed.
    pub signature_triggers: Vec<char>,
    /// The language server accent looked for and did not find, so the UI can say so when asked.
    pub missing: Option<String>,
}

// -------------------------------------------------------------------- tasks

/// A request in flight on the shared runtime. Dropping it aborts the request, which is what
/// sends `$/cancelRequest` to a language server.
///
/// A `JoinHandle` can be polled from any executor, so the GTK main loop awaits one directly.
pub struct Task<T>(tokio::task::JoinHandle<Result<T>>);

impl<T> Future for Task<T> {
    type Output = Result<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.0).poll(cx) {
            Poll::Ready(Ok(r)) => Poll::Ready(r),
            Poll::Ready(Err(e)) => Poll::Ready(Err(anyhow::anyhow!("{e}"))),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<T: Send + 'static> Task<T> {
    pub(crate) fn spawn(f: impl Future<Output = Result<T>> + Send + 'static) -> Task<T> {
        Task(accent_lsp::runtime().spawn(f))
    }

    /// For a call that blocks: a remote round trip over the ssh client.
    pub(crate) fn blocking(f: impl FnOnce() -> Result<T> + Send + 'static) -> Task<T> {
        Task(accent_lsp::runtime().spawn_blocking(f))
    }

    pub(crate) fn ready(value: Result<T>) -> Task<T> {
        Task::spawn(async move { value })
    }
}

// ----------------------------------------------------------------- providers

pub(crate) type Fut<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// One thing that answers for a set of open documents: a language server, or the index.
///
/// Boxed futures rather than `async fn`, because the registry holds these as `dyn Language`.
/// `open`, `change` and `close` are synchronous: each is a notification or a local analysis,
/// and neither provider waits on anything to answer them.
pub(crate) trait Language: Send + Sync {
    fn open(&self, rel: &str, language_id: &str, text: String) -> Result<Support>;
    fn change(&self, rel: &str, text: String) -> Result<()>;
    fn close(&self, rel: &str);
    fn completion(&self, rel: &str, pos: Pos, trigger: Option<char>) -> Fut<'_, Vec<Completion>>;
    fn resolve(&self, item: Completion) -> Fut<'_, Completion>;
    fn signature_help(&self, rel: &str, pos: Pos) -> Fut<'_, Option<Signature>>;
    fn hover(&self, rel: &str, pos: Pos) -> Fut<'_, Option<Hover>>;
    fn definition(&self, rel: &str, pos: Pos) -> Fut<'_, Vec<Location>>;
    fn symbols(&self, rel: &str) -> Fut<'_, Vec<Symbol>>;
    fn references(&self, rel: &str, pos: Pos) -> Fut<'_, Vec<Location>>;
    fn folds(&self, rel: &str) -> Fut<'_, Vec<Fold>>;
    /// The provider stopped answering (the server exited); the registry starts a fresh one.
    fn is_dead(&self) -> bool {
        false
    }
    fn shutdown(&self) -> Fut<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// A session that may still be starting: concurrent openers await the same cell.
pub(crate) type Session = Arc<tokio::sync::OnceCell<Arc<dyn Language>>>;

/// The providers a vault has started, and which document each one holds.
///
/// Filled in by the notes and external providers; the registry itself is the shape of the
/// session map and of the `Event` channel the diagnostics travel on.
pub(crate) struct Languages {
    pub(crate) root: PathBuf,
    pub(crate) db: PathBuf,
    pub(crate) events: Sender<Event>,
    /// One session per (server, root): the cell makes concurrent openers await one start.
    pub(crate) sessions: Mutex<HashMap<(String, PathBuf), Session>>,
    /// Open document → the provider holding it.
    pub(crate) docs: Mutex<HashMap<String, Arc<dyn Language>>>,
}

impl Languages {
    pub(crate) fn new(root: PathBuf, db: PathBuf, events: Sender<Event>) -> Arc<Languages> {
        Arc::new(Languages {
            root,
            db,
            events,
            sessions: Mutex::new(HashMap::new()),
            docs: Mutex::new(HashMap::new()),
        })
    }
}

/// Byte offset → position in `text`. Linear in the offset; a line table if a note ever makes it show.
pub fn pos_of(text: &str, byte: usize) -> Pos {
    let byte = byte.min(text.len());
    let head = &text[..byte];
    let line = head.matches('\n').count() as u32;
    let line_start = head.rfind('\n').map_or(0, |i| i + 1);
    Pos {
        line,
        character: head[line_start..].chars().count() as u32,
    }
}

/// Position → byte offset in `text`; `None` past the last line. A column past the line's end
/// lands on its end.
pub fn byte_of(text: &str, pos: Pos) -> Option<usize> {
    let mut start = 0;
    for _ in 0..pos.line {
        start = text[start..].find('\n').map(|i| start + i + 1)?;
    }
    let line_end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    let line = &text[start..line_end];
    let off = line
        .char_indices()
        .nth(pos.character as usize)
        .map_or(line.len(), |(i, _)| i);
    Some(start + off)
}

pub fn range_of(text: &str, r: &std::ops::Range<usize>) -> Range {
    Range {
        start: pos_of(text, r.start),
        end: pos_of(text, r.end),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_count_chars_not_bytes() {
        let text = "héllo\n[[wörld]]";
        assert_eq!(
            pos_of(text, 7),
            Pos {
                line: 1,
                character: 0
            }
        );
        assert_eq!(
            pos_of(text, 17),
            Pos {
                line: 1,
                character: 9
            }
        );
        assert_eq!(
            byte_of(
                text,
                Pos {
                    line: 1,
                    character: 9
                }
            ),
            Some(17)
        );
        assert_eq!(
            byte_of(
                text,
                Pos {
                    line: 0,
                    character: 99
                }
            ),
            Some(6)
        );
        assert_eq!(
            byte_of(
                text,
                Pos {
                    line: 5,
                    character: 0
                }
            ),
            None
        );
    }

    #[test]
    fn a_diagnostics_event_survives_the_wire() {
        let event = Event::Diagnostics {
            rel: "a.md".into(),
            items: vec![Diagnostic {
                range: Range::default(),
                severity: Severity::Hint,
                message: "No note named b".into(),
                source: Some("accent".into()),
            }],
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Event::Diagnostics { items, .. } if items.len() == 1));
    }
}
