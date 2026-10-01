//! The provider that answers for a source file: a real language server, over `accent-lsp`.
//!
//! Everything here is translation. The protocol counts columns in UTF-16 code units by default
//! and names files by URI; accent counts characters and names them by vault-relative path. So the
//! provider keeps the text of every open document — it needs it for the arithmetic anyway — and
//! each request is one conversion out and one back.
//!
//! The mapping functions are pure and sit above the provider, so what a server's answer becomes
//! is testable without a server.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::{Value, json};

use accent_lsp::types::{
    self, CompletionItem, CompletionResponse, CompletionTextEdit, DocumentSymbolResponse,
    Documentation, GotoDefinitionResponse, HoverContents, InlineCompletionResponse, MarkedString,
    ParameterLabel, PublishDiagnosticsParams, ServerCapabilities, SignatureHelp, on,
};
use accent_lsp::{Client, Notifications, from_uri, to_uri};

use super::{
    Completion, Completions, Diagnostic, Fold, Fut, Hover, Kind, Language, Location, Pos, Range,
    Severity, Signature, Support, Symbol, TextEdit, byte_of,
};
use crate::{Event, FileEdits, Local, locked};

/// A server that answered `-32801` was asked about a document it has already seen change. It is
/// the ordinary state of a fast typist, not a failure.
const CONTENT_MODIFIED: i64 = -32801;

// ------------------------------------------------------------------ position encoding

/// How the server on the other end counts a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encoding {
    Utf8,
    Utf16,
    Utf32,
}

impl Encoding {
    /// What the server said at `initialize`; absent means UTF-16, the protocol's default.
    pub(crate) fn parse(name: Option<&str>) -> Encoding {
        match name {
            Some("utf-8") => Encoding::Utf8,
            Some("utf-32") => Encoding::Utf32,
            _ => Encoding::Utf16,
        }
    }

    fn units(self, c: char) -> u32 {
        match self {
            Encoding::Utf8 => c.len_utf8() as u32,
            Encoding::Utf16 => c.len_utf16() as u32,
            Encoding::Utf32 => 1,
        }
    }

    /// A character column, as the server counts it.
    pub(crate) fn to_lsp(self, line: &str, ch: u32) -> u32 {
        line.chars().take(ch as usize).map(|c| self.units(c)).sum()
    }

    /// A column the server sent, as characters. A column past the line's end lands on its end,
    /// which is what a server does when it points at the newline.
    pub(crate) fn to_char(self, line: &str, unit: u32) -> u32 {
        let mut seen = 0;
        for (i, c) in line.chars().enumerate() {
            if seen >= unit {
                return i as u32;
            }
            seen += self.units(c);
        }
        line.chars().count() as u32
    }

    fn lsp_pos(self, text: &str, p: Pos) -> types::Position {
        types::Position {
            line: p.line,
            character: self.to_lsp(line_of(text, p.line), p.character),
        }
    }

    pub(crate) fn char_pos(self, text: &str, p: types::Position) -> Pos {
        Pos {
            line: p.line,
            character: self.to_char(line_of(text, p.line), p.character),
        }
    }

    fn char_range(self, text: &str, r: types::Range) -> Range {
        Range {
            start: self.char_pos(text, r.start),
            end: self.char_pos(text, r.end),
        }
    }
}

/// Line `n` of `text`, without its ending; empty when there is no such line.
pub(crate) fn line_of(text: &str, n: u32) -> &str {
    text.split('\n')
        .nth(n as usize)
        .unwrap_or_default()
        .trim_end_matches('\r')
}

/// The numbers as the server sent them, for a file whose text is not to hand: right for every
/// line that is plain ASCII, which is the only case where it is used.
fn raw_range(r: types::Range) -> Range {
    let at = |p: types::Position| Pos {
        line: p.line,
        character: p.character,
    };
    Range {
        start: at(r.start),
        end: at(r.end),
    }
}

// ------------------------------------------------------------------ the mapping

/// The identifier the caret sits at the end of: what a completion with no edit of its own
/// replaces.
fn word_start(line: &str, character: u32) -> u32 {
    let word = line
        .chars()
        .take(character as usize)
        .collect::<Vec<_>>()
        .iter()
        .rev()
        .take_while(|c| c.is_ascii_alphanumeric() || **c == '_')
        .count() as u32;
    character.saturating_sub(word)
}

/// What the popup is sorted by: the server's own key, or the label when it sent none.
fn sort_key(raw: &Value) -> String {
    raw.get("sortText")
        .or_else(|| raw.get("label"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn doc_text(d: Documentation) -> String {
    match d {
        Documentation::String(s) => s,
        Documentation::Markup(m) => m.value,
    }
}

fn edits_of(edits: Option<Vec<types::TextEdit>>, text: &str, enc: Encoding) -> Vec<TextEdit> {
    edits
        .unwrap_or_default()
        .into_iter()
        .map(|e| TextEdit {
            range: enc.char_range(text, e.range),
            text: e.new_text,
        })
        .collect()
}

/// One item, keeping the server's own object when it can still be resolved for more.
fn completion_of(
    raw: &Value,
    text: &str,
    pos: Pos,
    enc: Encoding,
    resolvable: bool,
) -> Option<Completion> {
    let item: CompletionItem = serde_json::from_value(raw.clone()).ok()?;
    let (insert, replace) = match item.text_edit {
        Some(CompletionTextEdit::Edit(e)) => (e.new_text, enc.char_range(text, e.range)),
        // The replacing range, not the inserting one: accepting a completion takes the word over.
        Some(CompletionTextEdit::InsertReplace(e)) => (e.new_text, enc.char_range(text, e.replace)),
        None => (
            item.insert_text.unwrap_or_else(|| item.label.clone()),
            Range {
                start: Pos {
                    line: pos.line,
                    character: word_start(line_of(text, pos.line), pos.character),
                },
                end: pos,
            },
        ),
    };
    Some(Completion {
        label: item.label,
        kind: item.kind.map_or(Kind::Text, Kind::from_lsp),
        detail: item.detail,
        doc: item.documentation.map(doc_text),
        filter: item.filter_text,
        insert,
        is_snippet: item.insert_text_format == Some(2),
        replace,
        extra_edits: edits_of(item.additional_text_edits, text, enc),
        resolve: resolvable.then(|| raw.clone()),
    })
}

fn completions_of(
    answer: Option<CompletionResponse>,
    text: &str,
    pos: Pos,
    enc: Encoding,
    resolvable: bool,
) -> Completions {
    let (mut items, incomplete) = match answer {
        Some(CompletionResponse::List(list)) => (list.items, list.is_incomplete),
        Some(CompletionResponse::Array(items)) => (items, false),
        None => return Completions::default(),
    };
    items.sort_by_cached_key(sort_key);
    Completions {
        items: items
            .iter()
            .filter_map(|raw| completion_of(raw, text, pos, enc, resolvable))
            .collect(),
        incomplete,
        pages: None,
    }
}

/// The line a ghost-text answer suggests, or nothing.
///
/// Only the first item is read: ghost text shows one suggestion, and cycling through several is
/// a UI that does not exist. A server that sends a `range` is answering about a span that starts
/// before the caret (Copilot rewrites the word being typed); what the buffer already holds there
/// is stripped, so the caller can always insert what comes back verbatim. A suggestion that does
/// not begin with what is already written is refused rather than guessed at.
fn inline_of(
    answer: Option<InlineCompletionResponse>,
    text: &str,
    pos: Pos,
    enc: Encoding,
) -> Option<String> {
    let items = match answer {
        Some(InlineCompletionResponse::List(list)) => list.items,
        Some(InlineCompletionResponse::Array(items)) => items,
        None => return None,
    };
    let item = items.into_iter().next()?;
    let suggestion = match item.range {
        None => item.insert_text,
        Some(range) => {
            let range = enc.char_range(text, range);
            let typed = between(text, range.start, pos)?;
            item.insert_text.strip_prefix(&typed)?.to_string()
        }
    };
    // ponytail: merl's own walk can answer a single space (its s1); a ghost of whitespace is
    // noise on screen either way, whoever sent it.
    match suggestion.trim().is_empty() {
        true => None,
        false => Some(suggestion),
    }
}

/// The text between two positions of the same document, or `None` where they do not name a span
/// of it: a range starting after the caret, or past the end.
fn between(text: &str, from: Pos, to: Pos) -> Option<String> {
    let from = super::byte_of(text, from)?;
    let to = super::byte_of(text, to)?;
    text.get(from..to).map(str::to_string)
}

fn marked(s: MarkedString) -> String {
    match s {
        MarkedString::String(s) => s,
        MarkedString::LanguageString { language, value } => format!("```{language}\n{value}\n```"),
    }
}

/// The three shapes of a hover flattened into one piece of markdown.
fn hover_text(c: HoverContents) -> String {
    match c {
        HoverContents::Markup(m) => m.value,
        HoverContents::Scalar(s) => marked(s),
        HoverContents::Array(v) => v.into_iter().map(marked).collect::<Vec<_>>().join("\n\n"),
    }
}

/// Only the signature the caret is in: the popover shows one, and picking it here keeps the
/// choice next to the `activeSignature` that decides it.
fn signature_of(help: SignatureHelp) -> Option<Signature> {
    let active = help.active_signature.unwrap_or(0) as usize;
    let sig = help.signatures.into_iter().nth(active)?;
    let params = sig
        .parameters
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| match p.label {
            // Offsets into the label are UTF-16 code units, whatever the document's encoding.
            ParameterLabel::Offsets([a, b]) => Some((
                Encoding::Utf16.to_char(&sig.label, a),
                Encoding::Utf16.to_char(&sig.label, b),
            )),
            ParameterLabel::Simple(name) => {
                let at = sig.label.find(&name)?;
                let start = sig.label[..at].chars().count() as u32;
                Some((start, start + name.chars().count() as u32))
            }
        })
        .collect();
    Some(Signature {
        label: sig.label,
        doc: sig.documentation.map(doc_text),
        params,
        active: sig.active_parameter.or(help.active_parameter),
    })
}

fn symbol_of(d: &types::DocumentSymbol, text: &str, enc: Encoding) -> Symbol {
    Symbol {
        name: d.name.clone(),
        range: enc.char_range(text, d.range),
        selection: enc.char_range(text, d.selection_range),
        children: d
            .children
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|c| symbol_of(c, text, enc))
            .collect(),
    }
}

fn symbols_of(answer: Option<DocumentSymbolResponse>, text: &str, enc: Encoding) -> Vec<Symbol> {
    match answer {
        Some(DocumentSymbolResponse::Nested(rows)) => {
            rows.iter().map(|d| symbol_of(d, text, enc)).collect()
        }
        // A flat answer knows only where the whole symbol is, so a jump lands on its start.
        Some(DocumentSymbolResponse::Flat(rows)) => rows
            .into_iter()
            .map(|s| {
                let range = enc.char_range(text, s.location.range);
                Symbol {
                    name: s.name,
                    range,
                    selection: range,
                    children: Vec::new(),
                }
            })
            .collect(),
        None => Vec::new(),
    }
}

/// Where a definition or a reference answer points, before the URIs become paths.
fn targets_of(answer: Option<GotoDefinitionResponse>) -> Vec<(String, types::Range)> {
    match answer {
        Some(GotoDefinitionResponse::Scalar(l)) => vec![(l.uri, l.range)],
        Some(GotoDefinitionResponse::Array(v)) => v.into_iter().map(|l| (l.uri, l.range)).collect(),
        Some(GotoDefinitionResponse::Link(v)) => v
            .into_iter()
            .map(|l| (l.target_uri, l.target_selection_range))
            .collect(),
        None => Vec::new(),
    }
}

fn severity_of(n: Option<u32>) -> Severity {
    match n {
        Some(2) => Severity::Warning,
        Some(3) => Severity::Info,
        Some(4) => Severity::Hint,
        // A server that names no severity means the worst of them.
        _ => Severity::Error,
    }
}

fn diagnostics_of(items: Vec<types::Diagnostic>, text: &str, enc: Encoding) -> Vec<Diagnostic> {
    items
        .into_iter()
        .map(|d| Diagnostic {
            range: enc.char_range(text, d.range),
            severity: severity_of(d.severity),
            message: d.message,
            source: d.source,
        })
        .collect()
}

/// One of a server's `willRename` filters, compiled once: a glob over the absolute path, and
/// whether it takes files (`Some(false)`), folders (`Some(true)`) or both.
type RenameFilter = (globset::GlobMatcher, Option<bool>);

/// The filters a server registered for `willRename`, leaving out any for a scheme other than
/// `file` and any glob that does not compile. `*` stops at a `/` and `**` does not, as the
/// protocol has it.
fn rename_filters(caps: &ServerCapabilities) -> Vec<RenameFilter> {
    let registered = caps
        .workspace
        .as_ref()
        .and_then(|w| w.file_operations.as_ref())
        .and_then(|f| f.will_rename.as_ref());
    let Some(registered) = registered else {
        return Vec::new();
    };
    registered
        .filters
        .iter()
        .filter(|f| f.scheme.as_deref().is_none_or(|scheme| scheme == "file"))
        .filter_map(|f| {
            let glob = globset::GlobBuilder::new(&f.pattern.glob)
                .literal_separator(true)
                .case_insensitive(f.pattern.options.as_ref().is_some_and(|o| o.ignore_case))
                .build()
                .ok()?
                .compile_matcher();
            let is_dir = match f.pattern.matches.as_deref() {
                Some("file") => Some(false),
                Some("folder") => Some(true),
                _ => None,
            };
            Some((glob, is_dir))
        })
        .collect()
}

/// Whether one of `filters` takes the file or folder at `abs`.
fn renamed_by(filters: &[RenameFilter], abs: &Path, is_dir: bool) -> bool {
    filters
        .iter()
        .any(|(glob, kind)| kind.is_none_or(|k| k == is_dir) && glob.is_match(abs))
}

/// A server's edits to one file as byte ranges of `text`, the positions read in the server's own
/// encoding. `None` when one lands past the last line, which makes `text` not the text the server
/// meant.
fn byte_edits(
    text: &str,
    edits: &[types::TextEdit],
    enc: Encoding,
) -> Option<Vec<(usize, usize, String)>> {
    edits
        .iter()
        .map(|e| {
            let r = enc.char_range(text, e.range);
            Some((
                byte_of(text, r.start)?,
                byte_of(text, r.end)?,
                e.new_text.clone(),
            ))
        })
        .collect()
}

/// A file the server named, as the rest of accent names it: vault-relative inside the vault,
/// absolute outside it.
fn rel_of(uri: &str, vault_root: &Path) -> Option<String> {
    let path = from_uri(uri)?;
    Some(match path.strip_prefix(vault_root) {
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    })
}

// ------------------------------------------------------------------ the provider

/// A document as the editor has it, which is what the server was last told about it.
struct Doc {
    version: i32,
    text: String,
}

type Docs = Arc<Mutex<HashMap<String, Doc>>>;

pub(crate) struct External {
    client: Client,
    caps: ServerCapabilities,
    encoding: Encoding,
    docs: Docs,
    renames: Vec<RenameFilter>,
    /// The vault root, for turning URIs into relative paths and back.
    root: PathBuf,
    /// The server is texlab, whose outline wants putting right ([`super::latex`]).
    texlab: bool,
    /// The builds' tables of contents that number texlab's outline.
    tocs: super::latex::Tocs,
}

/// Start a language server for `root` and hand back what answers with it.
///
/// `root` is the session root (a crate, a checkout); `vault_root` is what paths are relative to.
/// They differ whenever a vault holds more than one project.
/// Start `argv` and wire it up as a provider.
///
/// `busy_as` is what a background job in this server is called in the UI, or `None` for a server
/// whose progress is not worth showing — which is every language server: rust-analyzer reports
/// every `cargo check` and would never let the status bar settle. The ghost session is the one
/// that says something the reader wants, because its index is the reason a suggestion is missing.
pub(crate) async fn start(
    argv: Vec<String>,
    root: PathBuf,
    vault_root: PathBuf,
    events: Sender<Event>,
    busy_as: Option<&'static str>,
) -> Result<Arc<dyn Language>> {
    let name = argv.first().cloned().unwrap_or_default();
    let (client, notifications) =
        Client::spawn(&argv, &root).map_err(|e| anyhow::anyhow!("cannot start {name}: {e}"))?;
    let caps = client.initialize(&root).await?;
    let encoding = Encoding::parse(caps.position_encoding.as_deref());
    tracing::debug!("{name} started in {} ({encoding:?})", root.display());

    let docs: Docs = Arc::new(Mutex::new(HashMap::new()));
    // The task holds the documents and the channel, never the provider: it has to end when the
    // server's reader drops the sender, not when the last tab lets go of the session.
    accent_lsp::runtime().spawn(forward_notifications(
        notifications,
        docs.clone(),
        vault_root.clone(),
        encoding,
        busy_as,
        events,
    ));

    Ok(Arc::new(External {
        client,
        renames: rename_filters(&caps),
        caps,
        encoding,
        docs,
        root: vault_root,
        texlab: Path::new(&name)
            .file_stem()
            .is_some_and(|stem| stem == "texlab"),
        tocs: Default::default(),
    }))
}

/// Turn what the server publishes into events for the UI that can show it: diagnostics for the
/// tabs that paint them, and — for a server named by `busy_as` — whether it is busy.
///
/// A server analyses more than it was asked about — rust-analyzer publishes for every file in a
/// crate — and a diagnostic for a file nobody has open has nowhere to go, so it is dropped.
async fn forward_notifications(
    mut notifications: Notifications,
    docs: Docs,
    vault_root: PathBuf,
    encoding: Encoding,
    busy_as: Option<&'static str>,
    events: Sender<Event>,
) {
    // What this server has marks on screen for, so a crash can take exactly those away again.
    // A session that publishes nothing — the ghost one — therefore clears nothing.
    let mut painted: HashSet<String> = HashSet::new();
    while let Some(n) = notifications.recv().await {
        match n.method.as_str() {
            "textDocument/publishDiagnostics" => {
                let Ok(params) = serde_json::from_value::<PublishDiagnosticsParams>(n.params)
                else {
                    continue;
                };
                let Some(rel) = rel_of(&params.uri, &vault_root) else {
                    continue;
                };
                let Some(text) = locked(&docs).get(&rel).map(|d| d.text.clone()) else {
                    continue;
                };
                let items = diagnostics_of(params.diagnostics, &text, encoding);
                match items.is_empty() {
                    true => painted.remove(&rel),
                    false => painted.insert(rel.clone()),
                };
                if events.send(Event::Diagnostics { rel, items }).is_err() {
                    break;
                }
            }
            "$/progress" => {
                let Some(what) = busy_as else { continue };
                // `begin` and `end`; a `report` in between only refines a message nothing here
                // shows, so it is not a change of state.
                let busy = match n.params.pointer("/value/kind").and_then(Value::as_str) {
                    Some("begin") => true,
                    Some("end") => false,
                    _ => continue,
                };
                let what = what.to_string();
                tracing::debug!("{what}: busy={busy}");
                if events.send(Event::Busy { what, busy }).is_err() {
                    break;
                }
            }
            _ => continue,
        }
    }
    // The server exited, so whatever it was busy with is over. Without this a crash mid-index
    // would leave the status bar saying so for the life of the vault.
    if let Some(what) = busy_as {
        let _ = events.send(Event::Busy {
            what: what.to_string(),
            busy: false,
        });
    }
    // And nothing will ever correct what it painted: a crash leaves errors frozen in the gutter
    // of every document it had marked, which read as current until the tab is reopened. An empty
    // list is what clears one.
    for rel in painted {
        let _ = events.send(Event::Diagnostics {
            rel,
            items: Vec::new(),
        });
    }
}

impl External {
    /// The document as the editor last said it was.
    fn text_of(&self, rel: &str) -> Result<String> {
        locked(&self.docs)
            .get(rel)
            .map(|d| d.text.clone())
            .ok_or_else(|| anyhow::anyhow!("{rel} is not open"))
    }

    fn uri(&self, rel: &str) -> Result<String> {
        let path = Path::new(rel);
        Ok(match path.is_absolute() {
            true => to_uri(path),
            false => to_uri(&Local::join(&self.root, rel)?),
        })
    }

    fn doc_id(&self, rel: &str) -> Result<Value> {
        Ok(json!({"uri": self.uri(rel)?}))
    }

    /// A positional request's parameters, in the server's own encoding.
    fn at(&self, rel: &str, pos: Pos) -> Result<(Value, String)> {
        let text = self.text_of(rel)?;
        let p = self.encoding.lsp_pos(&text, pos);
        let params = json!({
            "textDocument": self.doc_id(rel)?,
            "position": {"line": p.line, "character": p.character},
        });
        Ok((params, text))
    }

    /// Where a URI the server named points, with the range converted against that file's text:
    /// the open document if it is one, else one read from disk. This runs on a click, not on a
    /// keystroke, so the read is affordable.
    fn location(&self, uri: &str, range: types::Range) -> Option<Location> {
        let path = from_uri(uri)?;
        let rel = rel_of(uri, &self.root)?;
        let text = self
            .text_of(&rel)
            .ok()
            .or_else(|| std::fs::read_to_string(&path).ok());
        Some(Location {
            range: match &text {
                Some(text) => self.encoding.char_range(text, range),
                None => raw_range(range),
            },
            path: rel,
            ..Location::default()
        })
    }

    /// A server's edits to `rel`, measured against the file on disk, which is what the server
    /// answered about: every open document is written out before a move is planned. One whose
    /// buffer still differs from its file is left alone rather than edited at positions that
    /// mean something else there.
    fn file_edits(&self, rel: &str, edits: &[types::TextEdit]) -> Option<FileEdits> {
        let (text, etag) = crate::fs::read_note(&Local::join(&self.root, rel).ok()?).ok()?;
        if locked(&self.docs)
            .get(rel)
            .is_some_and(|doc| doc.text != text)
        {
            return None;
        }
        Some(FileEdits {
            rel: rel.to_string(),
            etag,
            edits: byte_edits(&text, edits, self.encoding)?,
        })
    }

    fn support(&self) -> Support {
        let firsts =
            |triggers: &[String]| triggers.iter().filter_map(|t| t.chars().next()).collect();
        Support {
            completion_triggers: self
                .caps
                .completion_provider
                .as_ref()
                .map_or_else(Vec::new, |c| firsts(&c.trigger_characters)),
            signature_triggers: self
                .caps
                .signature_help_provider
                .as_ref()
                .map_or_else(Vec::new, |s| firsts(&s.trigger_characters)),
            missing: None,
            inline: on(&self.caps.inline_completion_provider),
        }
    }

    async fn ask<R: serde::de::DeserializeOwned + Default>(
        &self,
        method: &'static str,
        params: Value,
    ) -> Result<R> {
        match self.client.request::<R>(method, params).await {
            Ok(answer) => Ok(answer),
            // The document moved on while the server was answering about it; the next keystroke
            // asks again.
            Err(accent_lsp::Error::Response {
                code: CONTENT_MODIFIED,
                ..
            }) => {
                tracing::debug!("{method}: the document had already changed");
                Ok(R::default())
            }
            Err(e) => Err(anyhow::anyhow!("{method}: {e}")),
        }
    }
}

impl Language for External {
    fn open(&self, rel: &str, language_id: &str, text: String) -> Result<Support> {
        let uri = self.uri(rel)?;
        self.client.notify(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": uri, "languageId": language_id, "version": 1, "text": text,
            }}),
        )?;
        locked(&self.docs).insert(rel.to_string(), Doc { version: 1, text });
        Ok(self.support())
    }

    fn saved(&self, rel: &str) -> Result<()> {
        let uri = self.uri(rel)?;
        self.client.notify(
            "textDocument/didSave",
            json!({"textDocument": {"uri": uri}}),
        )?;
        Ok(())
    }

    fn change(&self, rel: &str, text: String) -> Result<()> {
        let uri = self.uri(rel)?;
        let version = {
            let mut docs = locked(&self.docs);
            let doc = docs
                .get_mut(rel)
                .ok_or_else(|| anyhow::anyhow!("{rel} is not open"))?;
            doc.version += 1;
            doc.text = text.clone();
            doc.version
        };
        // The whole text every time: full synchronisation is the one mode every server accepts,
        // and a note-sized file costs nothing to resend.
        self.client.notify(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": version},
                "contentChanges": [{"text": text}],
            }),
        )?;
        Ok(())
    }

    fn close(&self, rel: &str) {
        locked(&self.docs).remove(rel);
        if let Ok(uri) = self.uri(rel) {
            let _ = self.client.notify(
                "textDocument/didClose",
                json!({"textDocument": {"uri": uri}}),
            );
        }
    }

    fn completion(&self, rel: &str, pos: Pos, trigger: Option<char>) -> Fut<'_, Completions> {
        let rel = rel.to_string();
        Box::pin(async move {
            let Some(completion) = self.caps.completion_provider.as_ref() else {
                return Ok(Completions::default());
            };
            let (mut params, text) = self.at(&rel, pos)?;
            params["context"] = match trigger {
                Some(c) => json!({"triggerKind": 2, "triggerCharacter": c.to_string()}),
                None => json!({"triggerKind": 1}),
            };
            let answer = self.ask("textDocument/completion", params).await?;
            Ok(completions_of(
                answer,
                &text,
                pos,
                self.encoding,
                completion.resolve_provider,
            ))
        })
    }

    /// `triggerKind` is always 2, automatic: accent asks on a pause in the typing and has no
    /// action that asks for a suggestion, so there is never a 1 to send.
    fn inline_completion(&self, rel: &str, pos: Pos) -> Fut<'_, Option<String>> {
        let rel = rel.to_string();
        Box::pin(async move {
            if !on(&self.caps.inline_completion_provider) {
                return Ok(None);
            }
            let (mut params, text) = self.at(&rel, pos)?;
            params["context"] = json!({"triggerKind": 2});
            let answer = self.ask("textDocument/inlineCompletion", params).await?;
            Ok(inline_of(answer, &text, pos, self.encoding))
        })
    }

    fn resolve(&self, rel: &str, item: Completion) -> Fut<'_, Completion> {
        let rel = rel.to_string();
        Box::pin(async move {
            let (Some(raw), Ok(text)) = (item.resolve.clone(), self.text_of(&rel)) else {
                return Ok(item);
            };
            // Only the three properties accent asked to have resolved; everything else in the
            // answer is already in the item the popup is showing.
            match self
                .client
                .request::<CompletionItem>("completionItem/resolve", raw)
                .await
            {
                Ok(full) => Ok(Completion {
                    detail: full.detail.or(item.detail),
                    doc: full.documentation.map(doc_text).or(item.doc),
                    extra_edits: match full.additional_text_edits {
                        Some(edits) => edits_of(Some(edits), &text, self.encoding),
                        None => item.extra_edits,
                    },
                    ..item
                }),
                Err(e) => {
                    tracing::debug!("completionItem/resolve: {e}");
                    Ok(item)
                }
            }
        })
    }

    fn signature_help(&self, rel: &str, pos: Pos) -> Fut<'_, Option<Signature>> {
        let rel = rel.to_string();
        Box::pin(async move {
            if self.caps.signature_help_provider.is_none() {
                return Ok(None);
            }
            let (params, _) = self.at(&rel, pos)?;
            let answer: Option<SignatureHelp> =
                self.ask("textDocument/signatureHelp", params).await?;
            Ok(answer.and_then(signature_of))
        })
    }

    fn hover(&self, rel: &str, pos: Pos) -> Fut<'_, Option<Hover>> {
        let rel = rel.to_string();
        Box::pin(async move {
            if !types::on(&self.caps.hover_provider) {
                return Ok(None);
            }
            let (params, text) = self.at(&rel, pos)?;
            let answer: Option<types::Hover> = self.ask("textDocument/hover", params).await?;
            Ok(answer.and_then(|h| {
                let range = h.range.map(|r| self.encoding.char_range(&text, r));
                let text = hover_text(h.contents);
                (!text.trim().is_empty()).then_some(Hover { text, range })
            }))
        })
    }

    fn definition(&self, rel: &str, pos: Pos) -> Fut<'_, Vec<Location>> {
        let rel = rel.to_string();
        Box::pin(async move {
            if !types::on(&self.caps.definition_provider) {
                return Ok(Vec::new());
            }
            let (params, _) = self.at(&rel, pos)?;
            let answer = self.ask("textDocument/definition", params).await?;
            Ok(targets_of(answer)
                .into_iter()
                .filter_map(|(uri, range)| self.location(&uri, range))
                .collect())
        })
    }

    fn references(&self, rel: &str, pos: Pos) -> Fut<'_, Vec<Location>> {
        let rel = rel.to_string();
        Box::pin(async move {
            if !types::on(&self.caps.references_provider) {
                return Ok(Vec::new());
            }
            let (mut params, _) = self.at(&rel, pos)?;
            // The declaration belongs in the list: the pane is "everywhere this name is".
            params["context"] = json!({"includeDeclaration": true});
            let answer: Option<Vec<types::Location>> =
                self.ask("textDocument/references", params).await?;
            Ok(answer
                .unwrap_or_default()
                .into_iter()
                .filter_map(|l| self.location(&l.uri, l.range))
                .collect())
        })
    }

    fn symbols(&self, rel: &str) -> Fut<'_, Vec<Symbol>> {
        let rel = rel.to_string();
        Box::pin(async move {
            if !types::on(&self.caps.document_symbol_provider) {
                return Ok(Vec::new());
            }
            let text = self.text_of(&rel)?;
            let params = json!({"textDocument": self.doc_id(&rel)?});
            let answer = match self.ask("textDocument/documentSymbol", params).await? {
                Some(DocumentSymbolResponse::Nested(rows)) if self.texlab => {
                    let (own, near) = self.tocs.of(&self.root, &self.root.join(&rel));
                    let rows =
                        super::latex::tidy(rows, &text, own.as_deref(), &near, self.encoding);
                    Some(DocumentSymbolResponse::Nested(rows))
                }
                answer => answer,
            };
            Ok(symbols_of(answer, &text, self.encoding))
        })
    }

    fn folds(&self, rel: &str) -> Fut<'_, Vec<Fold>> {
        let rel = rel.to_string();
        Box::pin(async move {
            if !types::on(&self.caps.folding_range_provider) {
                return Ok(Vec::new());
            }
            let params = json!({"textDocument": self.doc_id(&rel)?});
            let answer: Option<Vec<types::FoldingRange>> =
                self.ask("textDocument/foldingRange", params).await?;
            Ok(answer
                .unwrap_or_default()
                .into_iter()
                // A fold that hides nothing is a gutter arrow that does nothing.
                .filter(|r| r.end_line > r.start_line)
                .map(|r| Fold {
                    start_line: r.start_line,
                    end_line: r.end_line,
                })
                .collect())
        })
    }

    fn renames(&self, abs: &Path, is_dir: bool) -> bool {
        renamed_by(&self.renames, abs, is_dir)
    }

    fn will_rename(&self, moves: Vec<(String, String)>) -> Fut<'_, Vec<FileEdits>> {
        Box::pin(async move {
            let files = moves
                .iter()
                .map(|(from, to)| Ok(json!({"oldUri": self.uri(from)?, "newUri": self.uri(to)?})))
                .collect::<Result<Vec<_>>>()?;
            let answer: Option<types::WorkspaceEdit> = self
                .ask("workspace/willRenameFiles", json!({"files": files}))
                .await?;
            let mut out = Vec::new();
            for (uri, edits) in answer
                .map(types::WorkspaceEdit::text_edits)
                .unwrap_or_default()
            {
                // Inside the vault only: a file elsewhere is not the vault's to change.
                let Some(rel) = rel_of(&uri, &self.root).filter(|rel| Path::new(rel).is_relative())
                else {
                    continue;
                };
                match self.file_edits(&rel, &edits) {
                    Some(found) => out.push(found),
                    None => tracing::warn!("left {rel} alone: it is not what the server read"),
                }
            }
            Ok(out)
        })
    }

    fn is_dead(&self) -> bool {
        self.client.is_dead()
    }

    fn shutdown(&self) -> Fut<'_, ()> {
        Box::pin(async move {
            self.client.shutdown().await;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// One character of each width, so every encoding disagrees about every column.
    const WIDE: &str = "aé😀b";

    #[test]
    fn a_column_means_the_same_place_in_every_encoding() {
        for (enc, units) in [
            (Encoding::Utf8, [0, 1, 3, 7]),
            (Encoding::Utf16, [0, 1, 2, 4]),
            (Encoding::Utf32, [0, 1, 2, 3]),
        ] {
            for (ch, unit) in units.iter().copied().enumerate() {
                let ch = ch as u32;
                assert_eq!(enc.to_lsp(WIDE, ch), unit, "{enc:?} char {ch}");
                assert_eq!(enc.to_char(WIDE, unit), ch, "{enc:?} unit {unit}");
            }
            // Past the end of the line is the end of the line, both ways.
            assert_eq!(enc.to_lsp(WIDE, 99), enc.to_lsp(WIDE, 4));
            assert_eq!(enc.to_char(WIDE, 99), 4);
        }
    }

    #[test]
    fn a_line_is_found_without_its_ending() {
        let text = "one\r\ntwo\nthree";
        assert_eq!(line_of(text, 0), "one");
        assert_eq!(line_of(text, 2), "three");
        assert_eq!(line_of(text, 9), "");
    }

    fn range(sl: u32, sc: u32, el: u32, ec: u32) -> Value {
        json!({"start": {"line": sl, "character": sc}, "end": {"line": el, "character": ec}})
    }

    fn parse<T: serde::de::DeserializeOwned>(v: Value) -> T {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_completion_takes_its_range_from_the_edit_or_from_the_word() {
        // Longer than the caret, so a replacing range can reach past it.
        let text = "let x = fool\n";
        let pos = Pos {
            line: 0,
            character: 10,
        };
        let items = json!([
            {"label": "format", "sortText": "a", "kind": 3,
             "textEdit": {"range": range(0, 8, 0, 10), "newText": "format!($1)"},
             "insertTextFormat": 2},
            {"label": "foo", "sortText": "b",
             "textEdit": {"newText": "foo", "insert": range(0, 8, 0, 10), "replace": range(0, 8, 0, 12)}},
            {"label": "fourth", "sortText": "c"},
        ]);
        let list = completions_of(parse(items), text, pos, Encoding::Utf16, true);
        assert!(!list.incomplete, "an array is the whole answer");
        let out = list.items;

        assert_eq!(
            out.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(),
            ["format", "foo", "fourth"],
            "sorted by sortText"
        );
        assert_eq!(out[0].insert, "format!($1)");
        assert!(out[0].is_snippet);
        assert_eq!(out[0].kind, Kind::Function);
        assert_eq!(out[0].replace.start.character, 8);
        assert_eq!(out[1].replace.end.character, 12, "the replacing range wins");
        assert!(!out[1].is_snippet);
        // No edit at all: the identifier before the caret.
        assert_eq!(out[2].insert, "fourth");
        assert_eq!(out[2].replace.start.character, 8);
        assert_eq!(out[2].replace.end, pos);
        assert!(out[2].resolve.is_some(), "kept for completionItem/resolve");
    }

    #[test]
    fn a_ghost_line_is_what_is_left_to_type() {
        let text = "naive caf\n";
        let pos = Pos {
            line: 0,
            character: 9,
        };

        // merl's shape: one item, no range, the rest of the line.
        let plain = json!({"items": [{"insertText": "e au lait"}]});
        assert_eq!(
            inline_of(parse(plain), text, pos, Encoding::Utf8),
            Some("e au lait".to_string())
        );

        // A range reaching back over the word: what is already written is stripped, so the
        // caller inserts what comes back at the caret either way.
        let over = json!({"items": [
            {"insertText": "caffe latte", "range": range(0, 6, 0, 9)}
        ]});
        assert_eq!(
            inline_of(parse(over), text, pos, Encoding::Utf8),
            Some("fe latte".to_string())
        );

        // A suggestion that does not continue what is written is refused, not guessed at.
        let elsewhere = json!({"items": [
            {"insertText": "tea", "range": range(0, 6, 0, 9)}
        ]});
        assert_eq!(inline_of(parse(elsewhere), text, pos, Encoding::Utf8), None);

        // Nothing to show: no items, a null answer, or a suggestion of pure whitespace.
        assert_eq!(
            inline_of(parse(json!({"items": []})), text, pos, Encoding::Utf8),
            None
        );
        assert_eq!(inline_of(None, text, pos, Encoding::Utf8), None);
        assert_eq!(
            inline_of(
                parse(json!([{"insertText": "  "}])),
                text,
                pos,
                Encoding::Utf8
            ),
            None
        );
    }

    #[test]
    fn a_capped_list_says_so() {
        let list = json!({"isIncomplete": true, "items": [{"label": "cite"}]});
        let out = completions_of(
            parse(list),
            "\\ci",
            Pos {
                line: 0,
                character: 3,
            },
            Encoding::Utf8,
            false,
        );
        assert!(out.incomplete);
        assert_eq!(out.items[0].label, "cite");
    }

    #[test]
    fn a_completion_range_is_converted_out_of_the_servers_units() {
        // `😀` is two UTF-16 units, so the server's column 5 is character 4.
        let text = "let 😀 = fo\n";
        let out = completions_of(
            parse(
                json!([{"label": "fo", "textEdit": {"range": range(0, 9, 0, 11), "newText": "fo"}}]),
            ),
            text,
            Pos {
                line: 0,
                character: 10,
            },
            Encoding::Utf16,
            false,
        ).items;
        assert_eq!(out[0].replace.start.character, 8);
        assert!(out[0].resolve.is_none(), "the server cannot resolve");
    }

    #[test]
    fn a_hover_reads_in_every_shape_it_arrives_in() {
        let markup: HoverContents = parse(json!({"kind": "markdown", "value": "**x**"}));
        assert_eq!(hover_text(markup), "**x**");
        let fenced: HoverContents = parse(json!({"language": "c", "value": "int add(int)"}));
        assert_eq!(hover_text(fenced), "```c\nint add(int)\n```");
        let many: HoverContents = parse(json!(["a", {"language": "c", "value": "b"}]));
        assert_eq!(hover_text(many), "a\n\n```c\nb\n```");
    }

    #[test]
    fn symbols_read_nested_and_flat() {
        let text = "int add(int a) { return a; }\n";
        let nested = symbols_of(
            Some(parse(json!([{
                "name": "add", "kind": 12, "range": range(0, 0, 0, 28), "selectionRange": range(0, 4, 0, 7),
                "children": [{"name": "a", "kind": 13, "range": range(0, 8, 0, 13), "selectionRange": range(0, 12, 0, 13)}]
            }]))),
            text,
            Encoding::Utf16,
        );
        assert_eq!(nested[0].name, "add");
        assert_eq!(nested[0].selection.start.character, 4);
        assert_eq!(nested[0].children[0].name, "a");

        let flat = symbols_of(
            Some(parse(json!([{
                "name": "add", "kind": 12, "location": {"uri": "file:///a.c", "range": range(0, 0, 0, 28)}
            }]))),
            text,
            Encoding::Utf16,
        );
        assert_eq!(
            flat[0].selection, flat[0].range,
            "a jump lands on its start"
        );
        assert!(flat[0].children.is_empty());
    }

    #[test]
    fn a_signature_labels_its_parameters_in_characters() {
        let help: SignatureHelp = parse(json!({
            "signatures": [{
                "label": "add(é: int, b: int)",
                "parameters": [{"label": [4, 10]}, {"label": "b: int"}],
                "activeParameter": 1
            }],
            "activeSignature": 0
        }));
        let sig = signature_of(help).unwrap();
        assert_eq!(sig.params, [(4, 10), (12, 18)]);
        assert_eq!(sig.active, Some(1));
    }

    #[test]
    fn published_diagnostics_become_an_event_with_character_columns() {
        let text = "int 😀 = addd();\n";
        let items = diagnostics_of(
            parse(json!([
                {"range": range(0, 9, 0, 13), "severity": 1, "message": "undeclared", "source": "clangd"},
                {"range": range(0, 0, 0, 3), "message": "no severity"},
            ])),
            text,
            Encoding::Utf16,
        );
        assert_eq!(items[0].severity, Severity::Error);
        assert_eq!(items[0].range.start.character, 8, "one UTF-16 unit fewer");
        assert_eq!(items[0].source.as_deref(), Some("clangd"));
        assert_eq!(
            items[1].severity,
            Severity::Error,
            "unsaid is the worst of them"
        );
    }

    #[test]
    fn a_uri_becomes_a_path_the_vault_knows() {
        let root = Path::new("/vault");
        assert_eq!(
            rel_of("file:///vault/src/a.c", root).as_deref(),
            Some("src/a.c")
        );
        assert_eq!(
            rel_of("file:///elsewhere/a.c", root).as_deref(),
            Some("/elsewhere/a.c")
        );
        assert_eq!(rel_of("https://example.org", root), None);
    }

    /// A server that exits — a crash, an OOM kill — leaves its marks in the gutter with nothing
    /// left to correct them. The notification stream ending is that moment, and no process is
    /// needed to reach it: dropping the sender is what a dead server's reader does.
    #[test]
    fn a_server_that_exits_clears_what_it_painted() {
        let (server, notifications) = tokio::sync::mpsc::unbounded_channel();
        let (events, seen) = std::sync::mpsc::channel();
        let text = "int main(void) { return addd(3); }\n";
        let docs: Docs = Arc::new(Mutex::new(HashMap::from([(
            "a.c".to_string(),
            Doc {
                version: 1,
                text: text.to_string(),
            },
        )])));
        server
            .send(accent_lsp::Notification {
                method: "textDocument/publishDiagnostics".to_string(),
                params: json!({
                    "uri": "file:///vault/a.c",
                    "diagnostics": [{
                        "range": {"start": {"line": 0, "character": 24},
                                  "end": {"line": 0, "character": 28}},
                        "severity": 1,
                        "message": "addd is not declared",
                    }],
                }),
            })
            .unwrap();
        drop(server);

        accent_lsp::runtime().block_on(forward_notifications(
            notifications,
            docs,
            PathBuf::from("/vault"),
            Encoding::Utf16,
            None,
            events,
        ));

        let painted = seen.recv().expect("the diagnostic the server published");
        assert!(matches!(painted, Event::Diagnostics { ref rel, ref items }
            if rel == "a.c" && items.len() == 1));
        let cleared = seen.recv().expect("the server's exit clears its marks");
        assert!(matches!(cleared, Event::Diagnostics { ref rel, ref items }
            if rel == "a.c" && items.is_empty()));
    }

    // ------------------------------------------------------------ with a real server

    /// A definition to jump to, a call that resolves, and a call that does not.
    const C_FILE: &str =
        "int add(int a, int b) { return a + b; }\nint main(void) { return add(1, 2) + addd(3); }\n";

    /// The clangd processes running right now, so the test can tell its own from everyone else's.
    fn clangd_pids() -> std::collections::BTreeSet<String> {
        std::process::Command::new("pgrep")
            .args(["-x", "clangd"])
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The one test with a language server actually running. Skipped where clangd is not
    /// installed, so a checkout without it still has a green suite.
    #[test]
    fn clangd_answers_about_a_c_file_and_leaves_nothing_behind() {
        if !super::super::in_path("clangd") {
            eprintln!("clangd is not installed: skipping the end-to-end test");
            return;
        }
        let before = clangd_pids();
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("main.c"), C_FILE).unwrap();
        let (vault, events) = crate::Vault::open_at(
            root.path(),
            &cache.path().join("index.db"),
            crate::VaultConfig::default(),
        )
        .unwrap();

        let rt = accent_lsp::runtime();
        rt.block_on(async {
            let support = vault
                .open_document("main.c", "c", C_FILE.to_string())
                .await
                .unwrap();
            assert_eq!(support.missing, None, "clangd is on PATH");
            assert!(support.completion_triggers.contains(&'.'));

            let names: Vec<String> = vault
                .symbols("main.c")
                .await
                .unwrap()
                .iter()
                .map(|s| s.name.clone())
                .collect();
            assert!(names.contains(&"add".to_string()) && names.contains(&"main".to_string()));

            // The caret inside the `add(1, 2)` call on the second line.
            let call = C_FILE.lines().nth(1).unwrap().find("add(1").unwrap();
            let at = Pos {
                line: 1,
                character: call as u32 + 1,
            };
            let to = vault.definition("main.c", at).await.unwrap();
            assert_eq!(to.len(), 1, "one definition of add");
            assert_eq!(to[0].path, "main.c");
            assert_eq!(to[0].range.start.line, 0);
        });

        // Diagnostics arrive when clangd has built its preamble, not when it is asked.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut error = None;
        while error.is_none() && std::time::Instant::now() < deadline {
            let Ok(event) = events.recv_timeout(std::time::Duration::from_millis(500)) else {
                continue;
            };
            if let Event::Diagnostics { rel, items } = event
                && rel == "main.c"
            {
                error = items.into_iter().find(|d| d.severity == Severity::Error);
            }
        }
        let error = error.expect("clangd has to complain about the call to addd");
        assert_eq!(error.range.start.line, 1);
        assert!(error.message.contains("addd"), "{}", error.message);

        // The last document to close takes the session with it: a server started for one file
        // must not sit on a vault nobody is reading code in any more.
        rt.block_on(async { vault.close_document("main.c").await.unwrap() });
        assert!(
            clangd_left(&before).is_empty(),
            "clangd outlived the only document it was started for"
        );
        // And reopening starts a fresh one, rather than asking the one that has exited.
        rt.block_on(async {
            let support = vault
                .open_document("main.c", "c", C_FILE.to_string())
                .await
                .unwrap();
            assert!(support.completion_triggers.contains(&'.'), "a new session");
        });

        // Dropping the vault stops the server: nothing of ours may outlive the window.
        drop(vault);
        assert!(
            clangd_left(&before).is_empty(),
            "clangd still running after the vault closed"
        );
    }

    /// The clangd processes this test started that are still running, given a little while to
    /// exit: a server is asked to stop and stops on its own thread.
    fn clangd_left(before: &std::collections::BTreeSet<String>) -> Vec<String> {
        let mut left = Vec::new();
        for _ in 0..30 {
            left = clangd_pids().difference(before).cloned().collect();
            if left.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        left
    }

    /// pylsp is the fallback of the Python row; pyright is tried first where it is installed.
    /// Completion is what the bare install answers (jedi is a hard dependency, the linters are
    /// extras), so that is what proves the session rather than a diagnostic.
    #[test]
    fn pylsp_answers_about_a_python_file() {
        if super::super::in_path("pyright-langserver") || !super::super::in_path("pylsp") {
            eprintln!("pylsp is not the Python server here: skipping the end-to-end test");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let py = "import os\nos.\n";
        std::fs::write(root.path().join("tool.py"), py).unwrap();
        let (vault, _events) = crate::Vault::open_at(
            root.path(),
            &cache.path().join("index.db"),
            crate::VaultConfig::default(),
        )
        .unwrap();
        accent_lsp::runtime().block_on(async {
            let support = vault
                .open_document("tool.py", "python3", py.to_string())
                .await
                .unwrap();
            assert_eq!(support.missing, None, "pylsp is on PATH");
            assert!(support.completion_triggers.contains(&'.'));
            let after_dot = Pos {
                line: 1,
                character: 3,
            };
            let items = vault
                .completion("tool.py", after_dot, Some('.'))
                .await
                .unwrap()
                .items;
            assert!(
                items.iter().any(|c| c.label == "path"),
                "os. should offer os.path, got {} items",
                items.len()
            );
        });
        drop(vault);
    }

    /// texlab answers the LaTeX row; what NOTEPAD asked for (`\\ref` and `\\cite` completion)
    /// is the server's own, so this is what proves it needs nothing accent-side.
    #[test]
    fn merl_suggests_the_rest_of_a_line_the_vault_has_written_before() {
        if !super::super::in_path("merl-rt") {
            eprintln!("merl-rt is not installed: skipping the ghost-text end-to-end test");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        // Twice, because a match has to have been seen more than once to be offered.
        let seen = "The kettle was already boiling.\n";
        std::fs::write(root.path().join("a.md"), format!("{seen}{seen}")).unwrap();
        let (vault, _events) = crate::Vault::open_at(
            root.path(),
            &cache.path().join("index.db"),
            crate::VaultConfig::default(),
        )
        .unwrap();

        let typed = "The kettle was";
        accent_lsp::runtime().block_on(async {
            let support = vault
                .open_document("b.md", "markdown", typed.to_string())
                .await
                .unwrap();
            assert!(support.inline, "merl answers for a note");
            let at = Pos {
                line: 0,
                character: typed.chars().count() as u32,
            };
            assert_eq!(
                vault.inline_completion("b.md", at).await.unwrap(),
                Some(" already boiling.".to_string()),
                "the rest of the line the vault already wrote"
            );

            // A note written after the session started is not in the index until the document
            // it was typed in settles.
            let later = "Rhubarb crumble needs custard.\n";
            std::fs::write(root.path().join("c.md"), format!("{later}{later}")).unwrap();
            let typed = "Rhubarb crumble";
            let at = Pos {
                line: 0,
                character: typed.chars().count() as u32,
            };
            vault
                .change_document("b.md", typed.to_string())
                .await
                .unwrap();
            assert_eq!(vault.inline_completion("b.md", at).await.unwrap(), None);
            vault.save_document("b.md").await.unwrap();
            vault.settle("b.md").await.unwrap();
            // The rebuild runs on a thread of merl's own, so the answer arrives once it lands.
            let mut got = None;
            for _ in 0..100 {
                got = vault.inline_completion("b.md", at).await.unwrap();
                if got.is_some() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            assert_eq!(
                got,
                Some(" needs custard.".to_string()),
                "settle re-indexed"
            );
        });
    }

    #[test]
    fn texlab_completes_labels_and_citations() {
        if !super::super::in_path("texlab") {
            eprintln!("texlab is not installed: skipping the end-to-end test");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let tex = "\\documentclass{article}\n\\begin{document}\n\\label{fig:abc}\n\\ref{fig:}\n\\cite{}\n\\ci\n\\bibliography{refs}\n\\input{}\n\\input{fig/}\n\\end{document}\n";
        std::fs::write(root.path().join("main.tex"), tex).unwrap();
        std::fs::create_dir(root.path().join("fig")).unwrap();
        for name in ["a.tex", "plot.pgf"] {
            std::fs::write(root.path().join("fig").join(name), "").unwrap();
        }
        std::fs::write(
            root.path().join("refs.bib"),
            "@article{knuth84, author = {Knuth}, title = {Literate Programming}, year = 1984}\n",
        )
        .unwrap();
        let (vault, events) = crate::Vault::open_at(
            root.path(),
            &cache.path().join("index.db"),
            crate::VaultConfig::default(),
        )
        .unwrap();
        // `\input{` lists the folder from the index.
        let reconciled = |e: &crate::Event| matches!(e, crate::Event::Reconciled(_));
        assert!(crate::tests::wait_for(&events, reconciled, crate::tests::BUDGET).is_some());
        accent_lsp::runtime().block_on(async {
            let support = vault
                .open_document("main.tex", "latex", tex.to_string())
                .await
                .unwrap();
            assert_eq!(support.missing, None, "texlab is on PATH");
            let labels: Vec<String> = vault
                .completion(
                    "main.tex",
                    Pos {
                        line: 3,
                        character: 9,
                    },
                    None,
                )
                .await
                .unwrap()
                .items
                .into_iter()
                .map(|c| c.label)
                .collect();
            assert!(labels.iter().any(|l| l == "fig:abc"), "labels: {labels:?}");
            let keys: Vec<String> = vault
                .completion(
                    "main.tex",
                    Pos {
                        line: 4,
                        character: 6,
                    },
                    None,
                )
                .await
                .unwrap()
                .items
                .into_iter()
                .map(|c| c.label)
                .collect();
            assert!(keys.iter().any(|k| k == "knuth84"), "citations: {keys:?}");
            // A command: texlab caps its answer at 50 and says so, which is what makes the
            // popup ask again as the name grows instead of narrowing a list that lacks `cite`.
            let after_backslash = vault
                .completion(
                    "main.tex",
                    Pos {
                        line: 5,
                        character: 1,
                    },
                    Some('\\'),
                )
                .await
                .unwrap();
            assert!(after_backslash.incomplete, "texlab caps the command list");
            let commands = vault
                .completion(
                    "main.tex",
                    Pos {
                        line: 5,
                        character: 3,
                    },
                    None,
                )
                .await
                .unwrap();
            assert!(
                commands.items.iter().any(|c| c.label == "cite"),
                "\\ci offers cite"
            );
            // texlab's `.tex` files and folders, and the other files beside them, once each.
            let input = |line, character| {
                let pos = Pos { line, character };
                let answer = vault.completion("main.tex", pos, Some('{'));
                async move {
                    let mut labels: Vec<String> = answer
                        .await
                        .unwrap()
                        .items
                        .into_iter()
                        .map(|c| c.label)
                        .collect();
                    labels.sort();
                    labels
                }
            };
            assert_eq!(input(7, 7).await, ["fig", "main.tex", "refs.bib"]);
            assert_eq!(input(8, 11).await, ["a.tex", "plot.pgf"]);
        });
        drop(vault);
    }

    /// Which moves a server wants to be asked about: its globs over the whole path, its kinds,
    /// its case rule, and only for files on disk.
    #[test]
    fn a_rename_is_asked_about_where_the_filters_take_it() {
        let caps: ServerCapabilities = parse(json!({"workspace": {"fileOperations": {
            "willRename": {"filters": [
                {"scheme": "file", "pattern": {"glob": "**/*.rs", "matches": "file"}},
                {"scheme": "file", "pattern": {"glob": "**", "matches": "folder"}},
                {"scheme": "untitled", "pattern": {"glob": "**/*.md"}},
                {"pattern": {"glob": "**/*.{ts,TSX}", "options": {"ignoreCase": true}}}
            ]}
        }}}));
        let filters = rename_filters(&caps);
        let takes = |path: &str, is_dir| renamed_by(&filters, Path::new(path), is_dir);
        assert!(takes("/v/src/foo.rs", false));
        assert!(!takes("/v/src/foo.rs.orig", false));
        assert!(takes("/v/src", true), "any folder");
        assert!(!takes("/v/src", false), "a file called src is no folder");
        assert!(!takes("/v/notes/a.md", false), "a scheme other than file");
        assert!(takes("/v/web/App.tsx", false), "ignoreCase");
        assert!(!takes("/v/tool.py", false));
        assert!(rename_filters(&ServerCapabilities::default()).is_empty());
    }

    /// An import edit lands on the bytes the server meant, whatever it counts columns in: `😀` is
    /// one character, two UTF-16 units and four bytes.
    #[test]
    fn import_edits_land_on_bytes_in_the_servers_encoding() {
        let text = "let s = \"😀\"; mod foo;\nuse foo::f;\n";
        let edits: Vec<types::TextEdit> = parse(json!([
            {"range": range(0, 18, 0, 21), "newText": "bar"},
            {"range": range(1, 4, 1, 7), "newText": "bar"},
        ]));
        let bytes = byte_edits(text, &edits, Encoding::Utf16).unwrap();
        assert_eq!(bytes[0], (20, 23, "bar".to_string()));
        let mut out = text.to_string();
        for (start, end, with) in bytes.iter().rev() {
            out.replace_range(*start..*end, with);
        }
        assert_eq!(out, "let s = \"😀\"; mod bar;\nuse bar::f;\n");
        // A line the text does not have: not the text the server read.
        let past: Vec<types::TextEdit> =
            parse(json!([{"range": range(9, 0, 9, 1), "newText": ""}]));
        assert_eq!(byte_edits(text, &past, Encoding::Utf16), None);
    }

    /// rust-analyzer's `willRenameFiles`: a module renamed in its own folder takes its `mod` and
    /// its `use` along — asked before the move, made after it — and one moved into another folder
    /// is asked about and answered with nothing, which is rust-analyzer's own limit.
    #[test]
    fn rust_analyzer_follows_a_renamed_module() {
        if !super::super::in_path("rust-analyzer") {
            eprintln!("rust-analyzer is not installed: skipping the end-to-end test");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("src")).unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        let lib = "mod foo;\npub use foo::f;\n";
        std::fs::write(root.path().join("src/lib.rs"), lib).unwrap();
        std::fs::write(root.path().join("src/foo.rs"), "pub fn f() {}\n").unwrap();

        let (vault, events) = crate::Vault::open_at(
            root.path(),
            &cache.path().join("index.db"),
            crate::VaultConfig::default(),
        )
        .unwrap();
        let budget = std::time::Duration::from_secs(60);
        assert!(
            crate::tests::wait_for(&events, |e| matches!(e, Event::Reconciled(_)), budget)
                .is_some()
        );
        accent_lsp::runtime().block_on(async {
            vault
                .open_document("src/lib.rs", "rust", lib.to_string())
                .await
                .unwrap();
        });
        // Until the crate graph has loaded the server knows no module to rename.
        let moves = [("src/foo.rs".to_string(), "src/bar.rs".to_string())];
        let started = std::time::Instant::now();
        let mut plan = vault.plan_moves(&moves).unwrap();
        while plan.imports.is_empty() && started.elapsed() < budget {
            std::thread::sleep(std::time::Duration::from_millis(500));
            plan = vault.plan_moves(&moves).unwrap();
        }
        eprintln!("willRenameFiles answered after {:?}", started.elapsed());
        assert!(plan.unchecked.is_empty(), "rust-analyzer was asked");
        assert_eq!(plan.imports.len(), 1, "{:?}", plan.imports);

        let away = [("src/foo.rs".to_string(), "src/sub/foo.rs".to_string())];
        let away = vault.plan_moves(&away).unwrap();
        assert!(away.imports.is_empty(), "{:?}", away.imports);
        assert!(
            away.unchecked.is_empty(),
            "asked, and answered with nothing"
        );

        let report = vault.rename(&plan, true).unwrap();
        assert_eq!(report.rewritten, ["src/lib.rs"]);
        assert_eq!(
            std::fs::read_to_string(root.path().join("src/lib.rs")).unwrap(),
            "mod bar;\npub use bar::f;\n"
        );
        drop(vault);
    }

    #[test]
    fn rust_analyzer_answers_about_a_crate() {
        if !super::super::in_path("rust-analyzer") {
            eprintln!("rust-analyzer is not installed: skipping the end-to-end test");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("src")).unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        let main = "fn add(a: i32, b: i32) -> i32 { a + b }\nfn main() { let s: i32 = \"no\"; println!(\"{}\", add(1, 2)); }\n";
        std::fs::write(root.path().join("src/main.rs"), main).unwrap();

        let (vault, events) = crate::Vault::open_at(
            root.path(),
            &cache.path().join("index.db"),
            crate::VaultConfig::default(),
        )
        .unwrap();
        accent_lsp::runtime().block_on(async {
            let support = vault
                .open_document("src/main.rs", "rust", main.to_string())
                .await
                .unwrap();
            assert_eq!(support.missing, None);
            // Symbols are answered from the syntax tree, so they do not wait for the crate graph.
            let names: Vec<String> = vault
                .symbols("src/main.rs")
                .await
                .unwrap()
                .iter()
                .map(|s| s.name.clone())
                .collect();
            assert_eq!(names, ["add", "main"]);
            // The compiler's diagnostics come from `cargo check`, which rust-analyzer runs on a
            // save and on nothing else: the whole reason the save is a notification of its own.
            vault.save_document("src/main.rs").await.unwrap();
        });
        // rust-analyzer's own type check reports first; the compiler's answer is the one whose
        // source is rustc, and it is the one that proves the save was heard.
        let started = std::time::Instant::now();
        let mut errors = Vec::new();
        while started.elapsed() < std::time::Duration::from_secs(60) && errors.is_empty() {
            let Ok(event) = events.recv_timeout(std::time::Duration::from_millis(500)) else {
                continue;
            };
            if let crate::Event::Diagnostics { rel, items } = event
                && rel == "src/main.rs"
            {
                errors = items
                    .into_iter()
                    .filter(|d| d.source.as_deref() == Some("rustc"))
                    .collect();
            }
        }
        eprintln!("cargo check diagnostics after {:?}", started.elapsed());
        assert!(
            errors
                .iter()
                .any(|d| d.message.contains("mismatched types")),
            "expected the type error from cargo check, got {errors:?}"
        );
        drop(vault);
    }
}
