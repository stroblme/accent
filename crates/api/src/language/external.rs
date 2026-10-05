//! The provider that answers for a source file: a real language server, over `accent-lsp`.
//!
//! Everything here is translation. The protocol counts columns in UTF-16 code units by default
//! and names files by URI; accent counts characters and names them by vault-relative path. So the
//! provider keeps the text of every open document — it needs it for the arithmetic anyway — and
//! each request is one conversion out and one back.
//!
//! The mapping functions are pure and live in [`map`], so what a server's answer becomes is
//! testable without a server.

pub(super) mod map;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::{Value, json};

use accent_lsp::types::{
    self, CompletionItem, DocumentSymbolResponse, PublishDiagnosticsParams, ServerCapabilities,
    SignatureHelp, on,
};
use accent_lsp::{Client, Notifications, from_uri, to_uri};

use super::{
    Completion, Completions, Fold, Fut, Hover, Language, Location, Pos, Signature, Support, Symbol,
};
use crate::{Event, FileEdits, Local, locked};
use map::{
    Encoding, RenameFilter, byte_edits, completions_of, diagnostics_of, doc_text, edits_of,
    hover_text, inline_of, raw_range, rel_of, rename_filters, renamed_by, signature_of, symbols_of,
    targets_of,
};

/// A server that answered `-32801` was asked about a document it has already seen change. It is
/// the ordinary state of a fast typist, not a failure.
const CONTENT_MODIFIED: i64 = -32801;

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
    // Whether it said it is busy and has not yet said it is done, likewise.
    let mut working = false;
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
                let Some((busy, message)) = progress_of(&n.params) else {
                    continue;
                };
                working = busy;
                let what = what.to_string();
                tracing::debug!("{what}: busy={busy} {message:?}");
                if events
                    .send(Event::Busy {
                        what,
                        busy,
                        message,
                    })
                    .is_err()
                {
                    break;
                }
            }
            _ => continue,
        }
    }
    // The server exited, so whatever it was busy with is over. Without this a crash or a shutdown
    // mid-index would leave the status bar saying so for the life of the vault. Only then: one
    // that exits idle has nothing to take back, and could take back a line its successor, started
    // in the meantime, has just put up.
    if let Some(what) = busy_as.filter(|_| working) {
        let _ = events.send(Event::Busy {
            what: what.to_string(),
            busy: false,
            message: None,
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

/// What a `$/progress` notification says: busy from its `begin` through each `report` and done
/// at its `end`, with the words either of the first two carry on how far it has got ("1200
/// files"). `None` for anything else.
fn progress_of(params: &Value) -> Option<(bool, Option<String>)> {
    let value = params.get("value")?;
    let busy = match value.get("kind")?.as_str()? {
        "begin" | "report" => true,
        "end" => false,
        _ => return None,
    };
    let message = value.get("message").and_then(Value::as_str);
    Some((busy, message.filter(|_| busy).map(str::to_string)))
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
    use crate::language::Severity;
    use serde_json::json;

    /// merl's `$/progress`, as `merl.c` sends it: busy from `begin` through every `report`, with
    /// the report's own words, and done at `end`.
    #[test]
    fn progress_says_busy_and_how_far() {
        let value = |v: Value| json!({"token": "merl/index", "value": v});
        assert_eq!(
            progress_of(&value(json!({"kind": "begin", "title": "Indexing"}))),
            Some((true, None))
        );
        assert_eq!(
            progress_of(&value(json!({"kind": "report", "message": "1200 files"}))),
            Some((true, Some("1200 files".to_string())))
        );
        assert_eq!(
            progress_of(&value(json!({"kind": "end"}))),
            Some((false, None))
        );
        assert_eq!(progress_of(&value(json!({"kind": "other"}))), None);
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
