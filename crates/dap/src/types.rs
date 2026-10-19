//! The bodies accent reads: answers, events and the adapter's `runInTerminal`. Accent's own
//! requests use JSON literals, as the language client's do; unknown fields are accepted and
//! ignored.
//!
//! Transcribed from the Debug Adapter Protocol's JSON schema,
//! <https://github.com/microsoft/debug-adapter-protocol> `debugAdapterProtocol.json`, which is
//!
//! MIT License
//!
//! Copyright (c) Microsoft Corporation.
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
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ------------------------------------------------------------------- the small shapes

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Source {
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Breakpoint {
    /// What a later `breakpoint` event names it by.
    pub id: Option<i64>,
    /// Bound, though maybe on another line than asked.
    pub verified: bool,
    /// Why it is not bound, for the reader.
    pub message: Option<String>,
    pub line: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StackFrame {
    /// Unique across threads; `scopes` and `evaluate` take it.
    pub id: i64,
    pub name: String,
    pub source: Option<Source>,
    /// 0, to be ignored, without a source.
    pub line: u32,
    /// In UTF-16 code units.
    pub column: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Thread {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Scope {
    /// `Locals`, `Globals`, shown as is.
    pub name: String,
    pub variables_reference: i64,
    /// Many variables, or slow to list: not opened unasked.
    pub expensive: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Variable {
    pub name: String,
    pub value: String,
    /// Sent when `initialize` said `supportsVariableType`.
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// Above 0, the value has children, listed by `variables`.
    pub variables_reference: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EvaluateResponse {
    pub result: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub variables_reference: i64,
}

// ------------------------------------------------------------------- the wrappers

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ThreadsResponse {
    pub threads: Vec<Thread>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StackTraceResponse {
    /// Innermost first.
    pub stack_frames: Vec<StackFrame>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScopesResponse {
    pub scopes: Vec<Scope>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VariablesResponse {
    pub variables: Vec<Variable>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SetBreakpointsResponse {
    /// In the order the request listed them.
    pub breakpoints: Vec<Breakpoint>,
}

// ------------------------------------------------------------------- events

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StoppedEventBody {
    /// `step`, `breakpoint`, `exception`, `pause`, …; shown when there is no `description`.
    pub reason: String,
    /// The whole reason, `Paused on exception`, shown as is.
    pub description: Option<String>,
    pub thread_id: Option<i64>,
    /// For an exception, its name.
    pub text: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OutputEventBody {
    /// `console`, `important`, `stdout`, `stderr`, `telemetry`; absent means `console`.
    pub category: Option<String>,
    pub output: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExitedEventBody {
    pub exit_code: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BreakpointEventBody {
    /// `changed`, `new` or `removed`.
    pub reason: String,
    /// Found by its `id`; the rest are its new values.
    pub breakpoint: Breakpoint,
}

// ------------------------------------------------------------------- the adapter's requests

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RunInTerminalRequestArguments {
    pub cwd: String,
    /// The command first.
    pub args: Vec<String>,
    /// Added to the environment; `null` takes a variable out.
    pub env: HashMap<String, Option<String>>,
}
