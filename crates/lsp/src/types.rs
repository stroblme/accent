//! The protocol shapes accent reads, transcribed from `lsp-types`.
//!
//! Only the answers are typed; everything accent *sends* is a `serde_json::json!` literal, which
//! keeps this file to the thirty-odd shapes that actually have to be understood instead of the
//! whole specification. Nothing here refuses an unknown field: servers send more than the
//! specification lists, and dropping the extras is the point.
//!
//! Transcribed from <https://github.com/gluon-lang/lsp-types>, which is
//!
//! MIT License
//!
//! Copyright (c) 2016 Markus Westerlind
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy of this software
//! and associated documentation files (the "Software"), to deal in the Software without
//! restriction, including without limitation the rights to use, copy, modify, merge, publish,
//! distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the
//! Software is furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all copies or
//! substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING
//! BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
//! NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
//! DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ------------------------------------------------------------------- the small shapes

/// A caret, counted in whatever unit the server said it encodes positions in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    pub uri: String,
    pub range: Range,
}

/// What a server answers instead of a [`Location`] when it knows both the definition and the part
/// of it worth selecting. Only the two fields accent uses are kept.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocationLink {
    pub target_uri: String,
    pub target_selection_range: Range,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextEdit {
    pub range: Range,
    pub new_text: String,
}

/// One edit with two ranges: `insert` when the completion should push the rest of the word along,
/// `replace` when it should take the word over.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InsertReplaceEdit {
    pub new_text: String,
    pub insert: Range,
    pub replace: Range,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkupContent {
    /// `markdown` or `plaintext`.
    pub kind: String,
    pub value: String,
}

// ------------------------------------------------------------------- initialize

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InitializeResult {
    pub capabilities: ServerCapabilities,
}

/// What the server admits to being able to do. The plain gates are kept as raw values because
/// each of them is either a boolean or an options object, and accent only ever asks whether it
/// is there — see [`on`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ServerCapabilities {
    /// `utf-8`, `utf-16` or `utf-32`; absent means UTF-16, the protocol's default.
    pub position_encoding: Option<String>,
    pub completion_provider: Option<CompletionOptions>,
    pub signature_help_provider: Option<SignatureHelpOptions>,
    pub hover_provider: Option<Value>,
    pub definition_provider: Option<Value>,
    pub references_provider: Option<Value>,
    pub document_symbol_provider: Option<Value>,
    pub folding_range_provider: Option<Value>,
}

/// Whether a capability gate is on: present, and not an explicit `false`.
pub fn on(v: &Option<Value>) -> bool {
    !matches!(v, None | Some(Value::Bool(false)))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CompletionOptions {
    pub trigger_characters: Vec<String>,
    pub resolve_provider: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SignatureHelpOptions {
    pub trigger_characters: Vec<String>,
    pub retrigger_characters: Vec<String>,
}

// ------------------------------------------------------------------- completion

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CompletionResponse {
    List(CompletionList),
    Array(Vec<Value>),
}

/// The items stay raw: `completionItem/resolve` wants the server's own object back, extra fields
/// and all, so accent parses a copy and keeps the original to send.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CompletionList {
    pub is_incomplete: bool,
    pub items: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionItem {
    pub label: String,
    pub kind: Option<u32>,
    pub detail: Option<String>,
    pub documentation: Option<Documentation>,
    pub sort_text: Option<String>,
    pub filter_text: Option<String>,
    pub insert_text: Option<String>,
    /// 1 is plain text, 2 a snippet.
    pub insert_text_format: Option<u32>,
    pub text_edit: Option<CompletionTextEdit>,
    pub additional_text_edits: Option<Vec<TextEdit>>,
    /// The server's own bookkeeping, handed back untouched on resolve.
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Documentation {
    String(String),
    Markup(MarkupContent),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CompletionTextEdit {
    /// Tried first: an [`InsertReplaceEdit`] has no `range`, so it cannot be mistaken for one.
    Edit(TextEdit),
    InsertReplace(InsertReplaceEdit),
}

// ------------------------------------------------------------------- hover and signatures

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hover {
    pub contents: HoverContents,
    pub range: Option<Range>,
}

/// Three shapes for one answer, two of them deprecated but still in wide use.
///
/// Order matters: `{kind, value}` must land on [`MarkupContent`] and `{language, value}` on a
/// [`MarkedString::LanguageString`], and neither struct has defaulted fields, so the first match
/// is the right one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HoverContents {
    Markup(MarkupContent),
    Scalar(MarkedString),
    Array(Vec<MarkedString>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MarkedString {
    String(String),
    LanguageString { language: String, value: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SignatureHelp {
    pub signatures: Vec<SignatureInformation>,
    pub active_signature: Option<u32>,
    pub active_parameter: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureInformation {
    pub label: String,
    pub documentation: Option<Documentation>,
    pub parameters: Option<Vec<ParameterInformation>>,
    pub active_parameter: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParameterInformation {
    pub label: ParameterLabel,
    pub documentation: Option<Documentation>,
}

/// Either the parameter's own text or where it sits inside the signature's label.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ParameterLabel {
    Simple(String),
    Offsets([u32; 2]),
}

// ------------------------------------------------------------------- navigation

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GotoDefinitionResponse {
    Scalar(Location),
    Array(Vec<Location>),
    Link(Vec<LocationLink>),
}

/// Nested when the server supports the hierarchy, flat when it does not.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DocumentSymbolResponse {
    Nested(Vec<DocumentSymbol>),
    Flat(Vec<SymbolInformation>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSymbol {
    pub name: String,
    pub detail: Option<String>,
    pub kind: u32,
    /// The whole symbol, body included.
    pub range: Range,
    /// Just its name, which is where a jump should land.
    pub selection_range: Range,
    pub children: Option<Vec<DocumentSymbol>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolInformation {
    pub name: String,
    pub kind: u32,
    pub location: Location,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FoldingRange {
    pub start_line: u32,
    pub end_line: u32,
    pub kind: Option<String>,
}

// ------------------------------------------------------------------- diagnostics

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PublishDiagnosticsParams {
    pub uri: String,
    pub version: Option<i32>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub range: Range,
    /// 1 error, 2 warning, 3 information, 4 hint.
    pub severity: Option<u32>,
    pub code: Option<Value>,
    pub source: Option<String>,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_completion_answer_reads_in_both_shapes() {
        let list: CompletionResponse =
            serde_json::from_value(json!({"isIncomplete": true, "items": [{"label": "a"}]}))
                .unwrap();
        let CompletionResponse::List(list) = list else {
            panic!("an object is a CompletionList")
        };
        assert!(list.is_incomplete);
        assert_eq!(list.items.len(), 1);

        let array: CompletionResponse =
            serde_json::from_value(json!([{"label": "a"}, {"label": "b"}])).unwrap();
        let CompletionResponse::Array(items) = array else {
            panic!("an array is a list of items")
        };
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn hover_contents_reads_in_all_three_shapes() {
        let markup: Hover =
            serde_json::from_value(json!({"contents": {"kind": "markdown", "value": "**x**"}}))
                .unwrap();
        assert!(matches!(markup.contents, HoverContents::Markup(m) if m.kind == "markdown"));

        let plain: Hover = serde_json::from_value(json!({"contents": "x"})).unwrap();
        assert!(matches!(
            plain.contents,
            HoverContents::Scalar(MarkedString::String(_))
        ));

        let fenced: Hover =
            serde_json::from_value(json!({"contents": {"language": "rust", "value": "fn x()"}}))
                .unwrap();
        assert!(
            matches!(fenced.contents, HoverContents::Scalar(MarkedString::LanguageString { language, .. }) if language == "rust")
        );

        let many: Hover =
            serde_json::from_value(json!({"contents": ["a", {"language": "c", "value": "b"}]}))
                .unwrap();
        assert!(matches!(many.contents, HoverContents::Array(v) if v.len() == 2));
    }

    #[test]
    fn document_symbols_read_nested_and_flat() {
        let range =
            json!({"start": {"line": 0, "character": 0}, "end": {"line": 1, "character": 0}});
        let nested: DocumentSymbolResponse = serde_json::from_value(json!([{
            "name": "main", "kind": 12, "range": range, "selectionRange": range,
            "children": [{"name": "inner", "kind": 13, "range": range, "selectionRange": range}]
        }]))
        .unwrap();
        let DocumentSymbolResponse::Nested(rows) = nested else {
            panic!("selectionRange makes it a DocumentSymbol")
        };
        assert_eq!(rows[0].children.as_ref().map(Vec::len), Some(1));

        let flat: DocumentSymbolResponse = serde_json::from_value(json!([{
            "name": "main", "kind": 12, "location": {"uri": "file:///a.c", "range": range}
        }]))
        .unwrap();
        let DocumentSymbolResponse::Flat(rows) = flat else {
            panic!("a location makes it a SymbolInformation")
        };
        assert_eq!(rows[0].name, "main");
    }

    #[test]
    fn a_capability_gate_is_off_only_when_absent_or_false() {
        assert!(!on(&None));
        assert!(!on(&Some(json!(false))));
        assert!(on(&Some(json!(true))));
        assert!(on(&Some(json!({"workDoneProgress": true}))));
    }
}
