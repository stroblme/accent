//! Code actions as a server sends them, and their edits as the window applies them: pure, so
//! testable without a server.

use std::collections::HashMap;
use std::path::Path;

use accent_lsp::types::WorkspaceEdit;
use serde_json::Value;

use super::map::{Encoding, byte_edits, rel_of};
use crate::language::{ActionEdits, CodeAction, TextEdit};
use crate::{Error, Etag, FileEdits, Result};

/// The actions of a `textDocument/codeAction` answer, commands and literals alike: a disabled one
/// left out, and an exact repeat once (rust-analyzer offers a fix once per diagnostic it fixes).
pub(super) fn actions_of(answer: Vec<Value>) -> Vec<CodeAction> {
    let mut out: Vec<CodeAction> = Vec::new();
    for raw in answer {
        if raw.get("disabled").is_some() || out.iter().any(|a| a.raw == raw) {
            continue;
        }
        let text = |key: &str| raw.get(key).and_then(Value::as_str).map(str::to_string);
        out.push(CodeAction {
            title: text("title").unwrap_or_default(),
            kind: text("kind").unwrap_or_default(),
            preferred: raw.get("isPreferred").and_then(Value::as_bool) == Some(true),
            raw,
        });
    }
    out
}

/// What `edits` change, as the window applies them: the documents open at the server (`open`, by
/// vault path: the version it was last sent and that text) in characters, every other file in
/// bytes of its text on disk (`read`). Refused whole for a file operation, a file outside the
/// vault, or an edit made for a version of an open document the server has since been sent.
pub(super) fn edits_of(
    edits: Vec<WorkspaceEdit>,
    open: &HashMap<String, (i32, String)>,
    root: &Path,
    encoding: Encoding,
    read: impl Fn(&str) -> Option<(String, Etag)>,
) -> Result<ActionEdits> {
    let refused = |why: String| Error::Language(format!("it {why}"));
    if edits.iter().any(WorkspaceEdit::has_file_operations) {
        return Err(refused(
            "creates, renames or deletes files, which a fix may not do yet".into(),
        ));
    }
    let mut out = ActionEdits::default();
    for (uri, version, list) in edits.into_iter().flat_map(WorkspaceEdit::text_edits) {
        let Some(rel) = rel_of(&uri, root).filter(|rel| Path::new(rel).is_relative()) else {
            return Err(refused("changes files outside the vault".into()));
        };
        match open.get(&rel) {
            Some((held, text)) => {
                if version.is_some_and(|v| v != *held) {
                    return Err(refused(format!("was made for an older {rel}")));
                }
                let list = list.into_iter().map(|e| TextEdit {
                    range: encoding.char_range(text, e.range),
                    text: e.new_text,
                });
                match out.open.iter_mut().find(|(r, _)| *r == rel) {
                    Some((_, edits)) => edits.extend(list),
                    None => out.open.push((rel, list.collect())),
                }
            }
            None => {
                let (text, etag) = read(&rel).ok_or_else(|| refused(format!("reads {rel}")))?;
                let bytes = byte_edits(&text, &list, encoding)
                    .ok_or_else(|| refused(format!("does not fit {rel} as it is")))?;
                match out.files.iter_mut().find(|f| f.rel == rel) {
                    Some(file) => file.edits.extend(bytes),
                    None => out.files.push(FileEdits {
                        rel,
                        etag,
                        edits: bytes,
                    }),
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::{Pos, Range};
    use serde_json::json;

    fn edit(line: u32, from: u32, to: u32, text: &str) -> Value {
        json!({"range": {"start": {"line": line, "character": from},
                         "end": {"line": line, "character": to}}, "newText": text})
    }

    fn parsed(edit: Value) -> WorkspaceEdit {
        serde_json::from_value(edit).unwrap()
    }

    /// clangd's twin rows, a disabled one and a bare command, as they arrive.
    #[test]
    fn actions_read_as_commands_and_literals_once_each() {
        let fix = json!({"title": "insert ';'", "kind": "quickfix", "isPreferred": true,
                         "edit": {"changes": {}}});
        let actions = actions_of(vec![
            fix.clone(),
            fix,
            json!({"title": "Extract", "kind": "refactor", "disabled": {"reason": "no"}}),
            json!({"title": "Swap", "command": "clangd.applyTweak", "arguments": []}),
        ]);
        let rows: Vec<_> = actions
            .iter()
            .map(|a| (a.title.as_str(), a.kind.as_str(), a.preferred))
            .collect();
        assert_eq!(
            rows,
            [("insert ';'", "quickfix", true), ("Swap", "", false)]
        );
    }

    /// The open document's edit in characters, an unopened file's in bytes of its disk text; a
    /// stale version, a file operation and a file outside the vault refuse the whole action.
    #[test]
    fn edits_go_to_the_open_document_and_to_files_on_disk() {
        let root = Path::new("/v");
        let open = HashMap::from([("a.c".to_string(), (2, "int é = 1\n".to_string()))]);
        let etag = Etag {
            mtime_ns: 1,
            size: 11,
            ino: 1,
        };
        let read = |rel: &str| (rel == "b.c").then(|| ("int other;\n".to_string(), etag));
        let both = parsed(json!({
            "changes": {"file:///v/a.c": [edit(0, 9, 9, ";")]},
            "documentChanges": [{"textDocument": {"uri": "file:///v/b.c", "version": null},
                                 "edits": [edit(0, 0, 0, "static ")]}]
        }));
        let out = edits_of(vec![both], &open, root, Encoding::Utf16, read).unwrap();
        let at = Pos {
            line: 0,
            character: 9,
        };
        assert_eq!(
            out.open,
            [(
                "a.c".to_string(),
                vec![TextEdit {
                    range: Range { start: at, end: at },
                    text: ";".into()
                }]
            )]
        );
        assert_eq!(out.files.len(), 1);
        assert_eq!(out.files[0].rel, "b.c");
        assert_eq!(out.files[0].edits, [(0, 0, "static ".to_string())]);

        let versioned = |version: i32| {
            parsed(json!({"documentChanges": [{
                "textDocument": {"uri": "file:///v/a.c", "version": version},
                "edits": [edit(0, 0, 3, "long")]}]}))
        };
        assert!(edits_of(vec![versioned(2)], &open, root, Encoding::Utf16, read).is_ok());
        let refused = [
            versioned(1),
            parsed(json!({"documentChanges": [{"kind": "create", "uri": "file:///v/c.c"}]})),
            parsed(json!({"changes": {"file:///usr/include/stdio.h": [edit(0, 0, 0, "x")]}})),
        ];
        for edit in refused {
            assert!(edits_of(vec![edit], &open, root, Encoding::Utf16, read).is_err());
        }
    }
}
