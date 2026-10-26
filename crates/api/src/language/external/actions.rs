//! Code actions as a server sends them, and their edits as the window applies them: pure, so
//! testable without a server.

use std::collections::HashMap;
use std::path::Path;

use accent_lsp::types::{self, CreateFile, WorkspaceEdit};
use serde_json::Value;

use super::map::{Encoding, byte_edits, rel_of};
use crate::language::{ActionEdits, CodeAction, Pos, Range, TextEdit};
use crate::local::applied;
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
            new_file: None,
        });
    }
    out
}

/// What `edits` change, as the window applies them: the documents open at the server (`open`, by
/// vault path: the version it was last sent and that text) in characters, every other file in
/// bytes of its text on disk (`read`), and the files they create with their text. Refused whole
/// for a rename or a delete, a file outside the vault, a file to create that is there already
/// where the server did not say to write over it or leave it, or an edit made for a version of an
/// open document the server has since been sent.
pub(super) fn edits_of(
    edits: Vec<WorkspaceEdit>,
    open: &HashMap<String, (i32, String)>,
    root: &Path,
    encoding: Encoding,
    read: impl Fn(&str) -> Option<(String, Etag)>,
) -> Result<ActionEdits> {
    let refused = |why: String| Error::Language(format!("it {why}"));
    let inside = |uri: &str| {
        rel_of(uri, root)
            .filter(|rel| Path::new(rel).is_relative())
            .ok_or_else(|| refused("changes files outside the vault".into()))
    };
    // The files it makes, whether each is one there already to write over, and their edits.
    let mut made: Vec<(String, bool, Vec<types::TextEdit>)> = Vec::new();
    for operation in edits.iter().flat_map(WorkspaceEdit::file_operations) {
        let create = (operation["kind"] == "create")
            .then(|| serde_json::from_value::<CreateFile>(operation.clone()).ok())
            .flatten()
            .ok_or_else(|| refused("renames or deletes files, which a fix may not do".into()))?;
        let rel = inside(&create.uri)?;
        let there = open.contains_key(&rel) || read(&rel).is_some();
        match (there, create.options.unwrap_or_default()) {
            (false, _) => made.push((rel, false, Vec::new())),
            (true, options) if options.overwrite => made.push((rel, true, Vec::new())),
            (true, options) if options.ignore_if_exists => {}
            (true, _) => return Err(refused(format!("creates {rel}, which is there already"))),
        }
    }
    let mut out = ActionEdits::default();
    for (uri, version, list) in edits.into_iter().flat_map(WorkspaceEdit::text_edits) {
        let rel = inside(&uri)?;
        if let Some((.., edits)) = made.iter_mut().find(|(r, ..)| *r == rel) {
            edits.extend(list);
            continue;
        }
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
    // A file made is the empty text with its edits made; one written over has all of its text
    // replaced by that, where it is held.
    for (rel, over, list) in made {
        let text = byte_edits("", &list, encoding)
            .and_then(|bytes| applied(String::new(), &bytes).ok())
            .ok_or_else(|| refused(format!("does not fit the new {rel}")))?;
        match (over, open.get(&rel)) {
            (false, _) => out.created.push((rel, text)),
            (true, Some((_, held))) => {
                let end = Pos {
                    line: held.matches('\n').count() as u32,
                    character: held.rsplit('\n').next().unwrap_or("").chars().count() as u32,
                };
                let range = Range {
                    start: Pos::default(),
                    end,
                };
                out.open.push((rel, vec![TextEdit { range, text }]));
            }
            (true, None) => {
                let (old, etag) = read(&rel).ok_or_else(|| refused(format!("reads {rel}")))?;
                out.files.push(FileEdits {
                    rel,
                    etag,
                    edits: vec![(0, old.len(), text)],
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
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
            parsed(json!({"documentChanges": [{"kind": "delete", "uri": "file:///v/b.c"}]})),
            parsed(json!({"changes": {"file:///usr/include/stdio.h": [edit(0, 0, 0, "x")]}})),
        ];
        for edit in refused {
            assert!(edits_of(vec![edit], &open, root, Encoding::Utf16, read).is_err());
        }
    }

    /// A file made is its edits made to empty text. One there already is refused, unless the
    /// server said to write over it, its whole text replaced, or to leave it, the edits made to it.
    #[test]
    fn a_file_created_is_written_new_and_one_there_is_refused() {
        let etag = Etag {
            mtime_ns: 1,
            size: 11,
            ino: 1,
        };
        let read = |rel: &str| (rel == "b.c").then(|| ("int other;\n".to_string(), etag));
        let made = |rel: &str, options: Value| {
            let uri = format!("file:///v/{rel}");
            let edit = parsed(json!({"documentChanges": [
                {"kind": "create", "uri": uri, "options": options},
                {"textDocument": {"uri": uri, "version": null}, "edits": [edit(0, 0, 0, "int b;\n")]}
            ]}));
            edits_of(
                vec![edit],
                &HashMap::new(),
                Path::new("/v"),
                Encoding::Utf16,
                read,
            )
        };
        let new = made("c.h", Value::Null).unwrap();
        assert_eq!(new.created, [("c.h".to_string(), "int b;\n".to_string())]);
        assert!(new.files.is_empty());
        assert!(made("b.c", json!({})).is_err());
        let over = made("b.c", json!({"overwrite": true, "ignoreIfExists": true})).unwrap();
        assert_eq!(over.files[0].edits, [(0, 11, "int b;\n".to_string())]);
        let left = made("b.c", json!({"ignoreIfExists": true})).unwrap();
        assert_eq!(left.files[0].edits, [(0, 0, "int b;\n".to_string())]);
    }
}
