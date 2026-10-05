//! The pure half of [`super`]: positions converted between the server's column units and
//! accent's characters, and what each of the server's answers becomes, testable without a server.

use std::path::Path;

use serde_json::Value;

use accent_lsp::from_uri;
use accent_lsp::types::{
    self, CompletionItem, CompletionResponse, CompletionTextEdit, DocumentSymbolResponse,
    Documentation, GotoDefinitionResponse, HoverContents, InlineCompletionResponse, MarkedString,
    ParameterLabel, ServerCapabilities, SignatureHelp,
};

use crate::language::{
    Completion, Completions, Diagnostic, Kind, Pos, Range, Severity, Signature, Symbol, TextEdit,
    byte_of,
};

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

    pub(super) fn lsp_pos(self, text: &str, p: Pos) -> types::Position {
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

    pub(super) fn char_range(self, text: &str, r: types::Range) -> Range {
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
pub(super) fn raw_range(r: types::Range) -> Range {
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

pub(super) fn doc_text(d: Documentation) -> String {
    match d {
        Documentation::String(s) => s,
        Documentation::Markup(m) => m.value,
    }
}

pub(super) fn edits_of(
    edits: Option<Vec<types::TextEdit>>,
    text: &str,
    enc: Encoding,
) -> Vec<TextEdit> {
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

pub(super) fn completions_of(
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
pub(super) fn inline_of(
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
    let from = byte_of(text, from)?;
    let to = byte_of(text, to)?;
    text.get(from..to).map(str::to_string)
}

fn marked(s: MarkedString) -> String {
    match s {
        MarkedString::String(s) => s,
        MarkedString::LanguageString { language, value } => format!("```{language}\n{value}\n```"),
    }
}

/// The three shapes of a hover flattened into one piece of markdown.
pub(super) fn hover_text(c: HoverContents) -> String {
    match c {
        HoverContents::Markup(m) => m.value,
        HoverContents::Scalar(s) => marked(s),
        HoverContents::Array(v) => v.into_iter().map(marked).collect::<Vec<_>>().join("\n\n"),
    }
}

/// Only the signature the caret is in: the popover shows one, and picking it here keeps the
/// choice next to the `activeSignature` that decides it.
pub(super) fn signature_of(help: SignatureHelp) -> Option<Signature> {
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

pub(super) fn symbols_of(
    answer: Option<DocumentSymbolResponse>,
    text: &str,
    enc: Encoding,
) -> Vec<Symbol> {
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
pub(super) fn targets_of(answer: Option<GotoDefinitionResponse>) -> Vec<(String, types::Range)> {
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

pub(super) fn diagnostics_of(
    items: Vec<types::Diagnostic>,
    text: &str,
    enc: Encoding,
) -> Vec<Diagnostic> {
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
pub(super) type RenameFilter = (globset::GlobMatcher, Option<bool>);

/// The filters a server registered for `willRename`, leaving out any for a scheme other than
/// `file` and any glob that does not compile. `*` stops at a `/` and `**` does not, as the
/// protocol has it.
pub(super) fn rename_filters(caps: &ServerCapabilities) -> Vec<RenameFilter> {
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
pub(super) fn renamed_by(filters: &[RenameFilter], abs: &Path, is_dir: bool) -> bool {
    filters
        .iter()
        .any(|(glob, kind)| kind.is_none_or(|k| k == is_dir) && glob.is_match(abs))
}

/// A server's edits to one file as byte ranges of `text`, the positions read in the server's own
/// encoding. `None` when one lands past the last line, which makes `text` not the text the server
/// meant.
pub(super) fn byte_edits(
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
pub(super) fn rel_of(uri: &str, vault_root: &Path) -> Option<String> {
    let path = from_uri(uri)?;
    Some(match path.strip_prefix(vault_root) {
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    })
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
}
