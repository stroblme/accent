//! The declarations in a source file — functions, methods, types and the like — found by a
//! scanner of our own: what the index keeps for a code file (its `symbols` table), and what an
//! outline, a caller or a callee is answered from without a language server.
//!
//! A scanner, not a parser. Comments and string literals are blanked first ([`mask`]), offsets
//! and newlines kept, so that no brace or keyword inside one is read as code. A brace language
//! is then read a statement at a time: what stands before a `{` is that block's header, and a
//! declaration is a header (or a statement ending in `;`) whose line a language's pattern names;
//! its end is the brace that closes the block. Python is read by indentation. What a pattern
//! does not know — a declaration a macro writes, a C function behind a `#if` — is not found, which
//! the scanner's tests measure against codegraph's tree-sitter parse of this repository.

use regex::{Regex, RegexSet};
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::sync::LazyLock;

/// The languages the scanner reads. TypeScript is read as JavaScript, C++ as C.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Lang {
    Rust,
    Python,
    Kotlin,
    Java,
    Script,
    Go,
    C,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SymbolKind {
    Function,
    /// A function inside a type: an `impl`, a trait, a class.
    Method,
    Struct,
    Enum,
    /// A trait, an interface.
    Trait,
    Class,
    TypeAlias,
    Module,
    Const,
    Macro,
}

impl SymbolKind {
    pub fn as_i64(self) -> i64 {
        self as i64
    }

    pub fn from_i64(v: i64) -> Self {
        use SymbolKind::*;
        [
            Function, Method, Struct, Enum, Trait, Class, TypeAlias, Module, Const, Macro,
        ]
        .get(v as usize)
        .copied()
        .unwrap_or(Function)
    }

    /// The word an outline shows it by.
    pub fn label(self) -> &'static str {
        match self {
            SymbolKind::Function => "fn",
            SymbolKind::Method => "method",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Trait => "trait",
            SymbolKind::Class => "class",
            SymbolKind::TypeAlias => "type",
            SymbolKind::Module => "mod",
            SymbolKind::Const => "const",
            SymbolKind::Macro => "macro",
        }
    }

    /// Whether it is called rather than named: what a call `name(` refers to.
    pub fn callable(self) -> bool {
        matches!(
            self,
            SymbolKind::Function | SymbolKind::Method | SymbolKind::Macro
        )
    }
}

/// One declaration the scanner found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decl {
    pub kind: SymbolKind,
    pub name: String,
    /// The type a method or an associated item belongs to: `Index` for `impl Index { fn … }`.
    pub container: Option<String>,
    /// From the start of the declaration's line to the end of its body, or of the statement for
    /// one without a body.
    pub range: Range<usize>,
    /// 1-based, the declaration's line and its body's last.
    pub line: u32,
    pub end_line: u32,
    /// The declaration up to its body, on one line.
    pub signature: String,
    /// Written as a test: `#[test]`, inside `#[cfg(test)]`, `def test_…`, `@Test`, `func Test…`.
    pub test: bool,
}

/// The language a file is read as, by its extension.
pub fn lang_of(rel: &str) -> Option<Lang> {
    let ext = rel.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => Lang::Rust,
        "py" | "pyi" => Lang::Python,
        "kt" | "kts" => Lang::Kotlin,
        "java" => Lang::Java,
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" => Lang::Script,
        "go" => Lang::Go,
        "c" | "h" | "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" | "c++" | "h++" => Lang::C,
        _ => return None,
    })
}

/// Whether a file is a test file by where it is or what it is called: everything declared in it
/// is a test's.
pub fn is_test_path(rel: &str) -> bool {
    let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
    let stem = name.split('.').next().unwrap_or(name);
    dir.split('/')
        .any(|d| matches!(d, "tests" | "test" | "__tests__" | "testing"))
        || stem == "tests"
        || stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with("Test")
        || name.contains(".test.")
        || name.contains(".spec.")
}

/// A line this long is a minified script or a generated table, not code to outline.
const MINIFIED: usize = 2_000;

/// The declarations in `text`, read as `lang`, in the order they start.
pub fn symbols(lang: Lang, text: &str) -> Vec<Decl> {
    if matches!(lang, Lang::Script | Lang::C) && text.lines().any(|l| l.len() > MINIFIED) {
        return Vec::new();
    }
    let masked = mask(lang, text);
    let mut out = match lang {
        Lang::Python => indented(&masked, text),
        _ => braced(lang, &masked, text),
    };
    if lang == Lang::C {
        out.extend(defines(&masked, text));
    }
    let starts = line_starts(text);
    let line = |byte: usize| starts.partition_point(|&s| s <= byte) as u32;
    for d in &mut out {
        d.line = line(d.range.start);
        d.end_line = line(d.range.end.saturating_sub(1).max(d.range.start));
    }
    out.sort_by_key(|d| d.range.start);
    out
}

/// The names a stretch of code calls — `name(`, `name::<T>(`, `name!(` — each once, in the
/// order it first calls them; `masked` as [`mask`] leaves it. A callee is looked for by these.
pub fn calls(masked: &str) -> Vec<&str> {
    static CALL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"([A-Za-z_]\w*)\s*(?:::\s*<[^<>()]*>\s*)?!?\s*\(").expect("a valid regex")
    });
    let mut out: Vec<&str> = Vec::new();
    for c in CALL.captures_iter(masked) {
        let name = c.get(1).expect("the name").as_str();
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

// ------------------------------------------------------------------------------------ masking

/// `text` with every comment and string or character literal blanked to spaces, newlines and
/// byte offsets kept: what the scanner reads, so that a `{` or a `fn` in one is not code.
pub fn mask(lang: Lang, text: &str) -> String {
    let b = text.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    // The last byte of code that is not a space: whether a JavaScript `/` divides or starts a
    // regular expression depends on it.
    let mut prev = b'\n';
    let mut prev_at = 0;
    while i < b.len() {
        let next = b.get(i + 1).copied().unwrap_or(0);
        let end = match (b[i], lang) {
            (b'#', Lang::Python) => Some(line_end(b, i)),
            (b'"' | b'\'', Lang::Python) => Some(python_string(b, i)),
            (b'/', Lang::Python) => None,
            (b'/', _) if next == b'/' => Some(line_end(b, i)),
            (b'/', _) if next == b'*' => {
                Some(block_end(b, i, matches!(lang, Lang::Rust | Lang::Kotlin)))
            }
            (b'"', Lang::Kotlin | Lang::Java) if b[i..].starts_with(b"\"\"\"") => {
                Some(find(b, i + 3, b"\"\"\"").map_or(b.len(), |e| e + 3))
            }
            (b'"', Lang::Rust) => Some(quoted(b, i, b'"', true)),
            (b'"', _) => Some(quoted(b, i, b'"', false)),
            (b'\'', Lang::Script) => Some(quoted(b, i, b'\'', false)),
            (b'\'', _) => char_literal(b, i),
            (b'`', Lang::Script) => Some(quoted(b, i, b'`', true)),
            (b'`', Lang::Go) => Some(find(b, i + 1, b"`").map_or(b.len(), |e| e + 1)),
            (b'r', Lang::Rust) if raw_prefix(b, i) => rust_raw(b, i),
            (b'R', Lang::C) if next == b'"' && !ident_before(b, i) => cpp_raw(b, i),
            (b'/', Lang::Script) if regex_may_start(b, prev, prev_at) => js_regex(b, i),
            _ => None,
        };
        match end {
            Some(end) => {
                for o in &mut out[i..end] {
                    if *o != b'\n' {
                        *o = b' ';
                    }
                }
                i = end;
            }
            None => {
                if !b[i].is_ascii_whitespace() {
                    prev = b[i];
                    prev_at = i;
                }
                i += 1;
            }
        }
    }
    String::from_utf8(out).expect("whole characters were replaced by ASCII spaces")
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

fn ident_before(b: &[u8], i: usize) -> bool {
    i > 0 && is_ident(b[i - 1])
}

fn line_end(b: &[u8], i: usize) -> usize {
    find(b, i, b"\n").unwrap_or(b.len())
}

fn find(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    b.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}

/// The end of the `/* … */` comment at `i`, nested ones counted where the language nests them.
fn block_end(b: &[u8], i: usize, nested: bool) -> usize {
    let mut depth = 0;
    let mut j = i;
    while j + 1 < b.len() {
        match (b[j], b[j + 1]) {
            (b'/', b'*') if nested || depth == 0 => {
                depth += 1;
                j += 2;
            }
            (b'*', b'/') => {
                depth -= 1;
                j += 2;
                if depth == 0 {
                    return j;
                }
            }
            _ => j += 1,
        }
    }
    b.len()
}

/// The end of the literal quoted by `q` at `i`, a backslash escaping what follows it; one that
/// may not span lines ends at the line's end unclosed.
fn quoted(b: &[u8], i: usize, q: u8, lines: bool) -> usize {
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            c if c == q => return j + 1,
            b'\n' if !lines => return j,
            _ => j += 1,
        }
    }
    b.len()
}

/// A character literal at `i` — `'a'`, `'\n'`, `'\u{1F600}'`, `'é'` — or `None` for what else a
/// quote starts: a Rust lifetime or label.
fn char_literal(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i + 1) == Some(&b'\\') {
        let close = (i + 3..b.len().min(i + 14)).find(|&j| b[j] == b'\'')?;
        return Some(close + 1);
    }
    let len = match *b.get(i + 1)? {
        c if c < 0x80 => 1,
        c if c >= 0xF0 => 4,
        c if c >= 0xE0 => 3,
        _ => 2,
    };
    (b.get(i + 1 + len) == Some(&b'\'')).then_some(i + 2 + len)
}

fn python_string(b: &[u8], i: usize) -> usize {
    let q = b[i];
    if b.get(i + 1) == Some(&q) && b.get(i + 2) == Some(&q) {
        let mut j = i + 3;
        while j + 2 < b.len() {
            match b[j] {
                b'\\' => j += 2,
                c if c == q && b[j + 1] == q && b[j + 2] == q => return j + 3,
                _ => j += 1,
            }
        }
        return b.len();
    }
    quoted(b, i, q, false)
}

/// Whether the `r` at `i` starts a raw string: `r"`, `r#"`, `br"`, `cr#"`, and not the end of
/// an identifier.
fn raw_prefix(b: &[u8], i: usize) -> bool {
    let quote_follows = b[i + 1..]
        .iter()
        .find(|&&c| c != b'#')
        .is_some_and(|&c| c == b'"');
    let prefixed = i > 0 && matches!(b[i - 1], b'b' | b'c') && !ident_before(b, i - 1);
    quote_follows && (!ident_before(b, i) || prefixed)
}

fn rust_raw(b: &[u8], i: usize) -> Option<usize> {
    let hashes = b[i + 1..].iter().take_while(|&&c| c == b'#').count();
    let open = i + 1 + hashes;
    if b.get(open) != Some(&b'"') {
        return None;
    }
    let mut close = vec![b'"'];
    close.extend(std::iter::repeat_n(b'#', hashes));
    Some(find(b, open + 1, &close).map_or(b.len(), |e| e + close.len()))
}

/// `R"delim( … )delim"`.
fn cpp_raw(b: &[u8], i: usize) -> Option<usize> {
    let paren = (i + 2..b.len().min(i + 20)).find(|&j| b[j] == b'(')?;
    let mut close = vec![b')'];
    close.extend_from_slice(&b[i + 2..paren]);
    close.push(b'"');
    Some(find(b, paren, &close).map_or(b.len(), |e| e + close.len()))
}

/// Whether a `/` after `prev` (at `prev_at`) starts a regular expression rather than dividing:
/// after an operator, an opening bracket or a keyword, not after a value.
fn regex_may_start(b: &[u8], prev: u8, prev_at: usize) -> bool {
    if b"(,=:[!&|?{};+-*%<>~^\n".contains(&prev) {
        return true;
    }
    if !is_ident(prev) {
        return false;
    }
    let start = (0..=prev_at)
        .rev()
        .take_while(|&j| is_ident(b[j]))
        .last()
        .unwrap_or(prev_at);
    matches!(
        &b[start..=prev_at],
        b"return"
            | b"typeof"
            | b"case"
            | b"do"
            | b"else"
            | b"in"
            | b"of"
            | b"yield"
            | b"await"
            | b"void"
            | b"delete"
            | b"throw"
            | b"new"
    )
}

/// The end of the regular expression literal at `i`, or `None` when the line ends before it
/// does, which makes it no literal.
fn js_regex(b: &[u8], i: usize) -> Option<usize> {
    let mut class = false;
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 1,
            b'[' => class = true,
            b']' => class = false,
            b'/' if !class => return Some(j + 1),
            b'\n' => return None,
            _ => {}
        }
        j += 1;
    }
    None
}

// ------------------------------------------------------------------------------ brace languages

/// What a block is to the declarations inside it.
#[derive(Clone, Copy, PartialEq)]
enum Scope {
    /// A type: a function directly inside it is its method.
    Type,
    /// A module or a namespace: what is inside is declared at its level.
    Module,
    /// A body or an expression: what is declared there is local.
    Body,
}

/// One block open while a brace language is read.
struct Open {
    /// The declaration the block is the body of, an index into the output.
    decl: Option<usize>,
    scope: Scope,
    /// What a [`Scope::Type`] is, and the type name it gives its methods.
    kind: Option<SymbolKind>,
    name: Option<String>,
    test: bool,
    /// Brackets open inside the block: a `;` or a newline inside them ends no statement.
    brackets: usize,
}

/// A declaration a pattern names in a header.
struct Found {
    kind: SymbolKind,
    /// Empty for a pattern that opens a scope and declares nothing: an `impl`, a namespace.
    name: String,
    /// A Go method's receiver type.
    container: Option<String>,
    /// Where the declaration's line starts, and its first character.
    line: usize,
    at: usize,
    /// What the block it opens is.
    scope: Scope,
}

fn braced(lang: Lang, masked: &str, text: &str) -> Vec<Decl> {
    let b = masked.as_bytes();
    let mut out: Vec<Decl> = Vec::new();
    let mut stack = vec![Open {
        decl: None,
        scope: Scope::Module,
        kind: None,
        name: None,
        test: false,
        brackets: 0,
    }];
    // Where the statement being read began, and the line being read.
    let mut chunk = 0;
    let mut line_start = 0;
    // Kotlin, Go and JavaScript end a statement at a line's end as well.
    let newline_ends = matches!(lang, Lang::Kotlin | Lang::Go | Lang::Script);
    for (i, &c) in b.iter().enumerate() {
        let top = stack.last_mut().expect("the file's own scope stays");
        match c {
            b'(' | b'[' => top.brackets += 1,
            b')' | b']' => top.brackets = top.brackets.saturating_sub(1),
            b'{' => {
                let found = find_decl(lang, masked, chunk..i, top);
                let mut open = Open {
                    decl: None,
                    scope: Scope::Body,
                    kind: None,
                    name: None,
                    test: top.test || test_attr(lang, &masked[chunk..i]),
                    brackets: 0,
                };
                if let Some(f) = found {
                    open.scope = f.scope;
                    open.kind = Some(f.kind);
                    open.test |= f.kind == SymbolKind::Module && f.name == "tests";
                    if f.scope == Scope::Type {
                        open.name = Some(match f.name.is_empty() {
                            false => f.name.clone(),
                            true => scope_type(lang, &masked[f.at..i], &stack),
                        });
                    }
                    if !f.name.is_empty() {
                        open.decl = Some(out.len());
                        out.push(decl(&stack, f, i + 1, text, i, open.test));
                    }
                }
                stack.push(open);
                chunk = i + 1;
            }
            b'}' => {
                if stack.len() > 1 {
                    let open = stack.pop().expect("more than one");
                    if let Some(d) = open.decl {
                        out[d].range.end = i + 1;
                    }
                }
                chunk = i + 1;
            }
            b';' if top.brackets == 0 => {
                let head = &masked[chunk..i];
                // A Java method ends in `;` only in an interface or when abstract or native; an
                // enum's constants `A(1), B(2);` are none.
                let java_call = lang == Lang::Java
                    && top.kind != Some(SymbolKind::Trait)
                    && !head.contains("abstract")
                    && !head.contains("native");
                // Nor is a C function ending in `;` a definition: a prototype or a call.
                let call = java_call || lang == Lang::C;
                if let Some(f) = find_decl(lang, masked, chunk..i, top).filter(|f| {
                    !f.name.is_empty()
                        && f.kind != SymbolKind::Module
                        && !(call && f.kind == SymbolKind::Function)
                }) {
                    let test = top.test || test_attr(lang, head);
                    out.push(decl(&stack, f, i + 1, text, i, test));
                }
                chunk = i + 1;
            }
            b'\n' if newline_ends && top.brackets == 0 => {
                // A blank line, and a line ending mid-expression, carry the statement on to the
                // line after it; so does an annotation, to the declaration under it.
                let line = masked[line_start..i].trim();
                let head = masked[chunk..i].trim();
                let tail = &text[chunk + masked[chunk..i].trim_end().len()..i];
                if line.is_empty() || continues(head, tail) {
                    line_start = i + 1;
                    continue;
                }
                let found = find_decl(lang, masked, chunk..i, top)
                    .filter(|f| !f.name.is_empty() && bodyless(lang, f.kind, head));
                line_start = i + 1;
                match found {
                    Some(f) => {
                        let test = top.test || test_attr(lang, &masked[chunk..i]);
                        out.push(decl(&stack, f, i, text, i, test));
                    }
                    None if line.starts_with('@') => continue,
                    None => {}
                }
                chunk = i + 1;
            }
            b'\n' => line_start = i + 1,
            _ => {}
        }
    }
    out
}

/// Whether a line ending so leaves its statement open: a list, an assignment, an operator.
/// `tail` is what the masking blanked after the code's last character: a string there ends the
/// line, a comment does not.
fn continues(head: &str, tail: &str) -> bool {
    let string = tail.trim_start().starts_with(['"', '\'', '`']);
    !string
        && (head.ends_with([',', ':', '=', '+', '-', '*', '/', '|', '&', '.', '<', '?'])
            || head.ends_with("->")
            || head.ends_with("=>"))
}

/// Whether a declaration a line ends is one without a body, rather than a header whose `{` is
/// on a later line.
fn bodyless(lang: Lang, kind: SymbolKind, head: &str) -> bool {
    match lang {
        // `fun f() = 1`, an abstract `fun f()`, `class A(val x: Int)`, `typealias`.
        Lang::Kotlin => true,
        // `type ID int`, `type A = B`.
        Lang::Go => kind == SymbolKind::TypeAlias,
        // `type A = B`, an overload's signature.
        _ => kind == SymbolKind::TypeAlias || head.ends_with(')') && kind.callable(),
    }
}

/// The declaration `f`, its body or statement ending at `end`, its signature what stands from
/// its line's first character to `header_end`.
fn decl(stack: &[Open], f: Found, end: usize, text: &str, header_end: usize, test: bool) -> Decl {
    let top = stack.last().expect("the file's own scope stays");
    let (kind, container) = match (f.kind, top.scope, f.container) {
        (_, _, Some(receiver)) => (SymbolKind::Method, Some(receiver)),
        (SymbolKind::Function, Scope::Type, None) => (SymbolKind::Method, top.name.clone()),
        (kind, Scope::Type, None) => (kind, top.name.clone()),
        (kind, _, None) => (kind, None),
    };
    let line = &text[f.line..header_end.max(f.line)];
    let mut signature = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some((cut, _)) = signature.char_indices().nth(200) {
        signature.truncate(cut);
        signature.push('…');
    }
    Decl {
        kind,
        name: f.name,
        container,
        range: f.line..end,
        line: 0,
        end_line: 0,
        signature,
        test,
    }
}

/// Whether the attributes or annotations before a declaration mark it a test.
fn test_attr(lang: Lang, head: &str) -> bool {
    static RUST: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"#\[(?:[\w:]+::)?(?:test|rstest)\b|#\[cfg\(test\)\]").expect("a valid regex")
    });
    match lang {
        Lang::Rust => RUST.is_match(head),
        Lang::Kotlin | Lang::Java => head.contains("@Test") || head.contains("@ParameterizedTest"),
        Lang::Go => head.contains("*testing.T") || head.contains("*testing.B"),
        _ => false,
    }
}

/// A language's declaration pattern: the kind it names, and the block it opens. One with no
/// `name` group opens a scope without declaring anything.
struct Pattern {
    re: String,
    kind: SymbolKind,
    scope: Scope,
    /// Only directly inside a type: a Java or JavaScript method, which elsewhere is a call.
    member: bool,
}

fn pattern(re: impl Into<String>, kind: SymbolKind, scope: Scope) -> Pattern {
    Pattern {
        re: re.into(),
        kind,
        scope,
        member: false,
    }
}

fn member(re: impl Into<String>, kind: SymbolKind) -> Pattern {
    Pattern {
        member: true,
        ..pattern(re, kind, Scope::Body)
    }
}

/// A language's patterns compiled, and as one set that rules most statements out in one pass.
struct Patterns {
    set: RegexSet,
    each: Vec<(Regex, Pattern)>,
}

impl Patterns {
    fn new(list: Vec<Pattern>) -> Patterns {
        let res: Vec<String> = list.iter().map(|p| format!("(?m){}", p.re)).collect();
        Patterns {
            set: RegexSet::new(&res).expect("valid declaration patterns"),
            each: res
                .iter()
                .zip(list)
                .map(|(re, p)| (Regex::new(re).expect("a valid declaration pattern"), p))
                .collect(),
        }
    }
}

use Scope::{Body, Type};
use SymbolKind::*;

/// Rust's item prefix: attributes on the line, visibility.
const RS: &str = r"^[ \t]*(?:#\[[^\n]*\][ \t]*)*(?:pub(?:[ \t]*\([^)\n]*\))?[ \t]+)?";

static RUST: LazyLock<Patterns> = LazyLock::new(|| {
    Patterns::new(vec![
        pattern(
            format!(
                r"{RS}(?:(?:const|async|unsafe|safe|default|extern)[ \t]+)*fn[ \t]+(?P<name>r#\w+|[A-Za-z_]\w*)"
            ),
            Function,
            Body,
        ),
        pattern(
            format!(r"{RS}(?:struct|union)[ \t]+(?P<name>\w+)"),
            Struct,
            Type,
        ),
        pattern(format!(r"{RS}enum[ \t]+(?P<name>\w+)"), Enum, Type),
        pattern(
            format!(r"{RS}(?:unsafe[ \t]+)?(?:auto[ \t]+)?trait[ \t]+(?P<name>\w+)"),
            Trait,
            Type,
        ),
        pattern(format!(r"{RS}type[ \t]+(?P<name>\w+)"), TypeAlias, Body),
        pattern(
            format!(r"{RS}(?:const|static(?:[ \t]+mut)?)[ \t]+(?P<name>[A-Za-z]\w*)[ \t]*:"),
            Const,
            Body,
        ),
        pattern(
            format!(r"{RS}mod[ \t]+(?P<name>\w+)"),
            SymbolKind::Module,
            Scope::Module,
        ),
        pattern(
            r"^[ \t]*(?:#\[[^\n]*\][ \t]*)*macro_rules![ \t]*(?P<name>\w+)",
            Macro,
            Body,
        ),
        pattern(r"^[ \t]*(?:unsafe[ \t]+)?impl\b", Class, Type),
    ])
});

/// Kotlin's modifiers, annotations on the line among them.
const KT: &str = r"^[ \t]*(?:(?:@[\w.]+(?:\([^)\n]*\))?|public|private|protected|internal|override|open|abstract|final|sealed|data|inner|annotation|value|inline|suspend|operator|infix|tailrec|external|actual|expect|const|lateinit)[ \t]+)*";

static KOTLIN: LazyLock<Patterns> = LazyLock::new(|| {
    Patterns::new(vec![
        pattern(
            format!(
                r"{KT}fun[ \t]+(?:<[^>\n]*>[ \t]*)?(?:[\w.<>?, ]+\.)?(?P<name>\w+|`[^`\n]+`)[ \t]*\("
            ),
            Function,
            Body,
        ),
        pattern(
            format!(r"{KT}enum[ \t]+class[ \t]+(?P<name>\w+)"),
            Enum,
            Type,
        ),
        pattern(
            format!(r"{KT}(?:class|object)[ \t]+(?P<name>\w+)"),
            Class,
            Type,
        ),
        pattern(
            format!(r"{KT}(?:fun[ \t]+)?interface[ \t]+(?P<name>\w+)"),
            Trait,
            Type,
        ),
        pattern(
            format!(r"{KT}typealias[ \t]+(?P<name>\w+)"),
            TypeAlias,
            Body,
        ),
        pattern(r"^[ \t]*companion[ \t]+object\b", Class, Type),
    ])
});

const JAVA_MODS: &str = r"^[ \t]*(?:(?:@[\w.]+(?:\([^)\n]*\))?|public|private|protected|static|final|abstract|sealed|non-sealed|strictfp|synchronized|native|default|transient)[ \t]+)*";

static JAVA: LazyLock<Patterns> = LazyLock::new(|| {
    Patterns::new(vec![
        pattern(format!(r"{JAVA_MODS}enum[ \t]+(?P<name>\w+)"), Enum, Type),
        pattern(
            format!(r"{JAVA_MODS}(?:class|record)[ \t]+(?P<name>\w+)"),
            Class,
            Type,
        ),
        pattern(
            format!(r"{JAVA_MODS}@?interface[ \t]+(?P<name>\w+)"),
            Trait,
            Type,
        ),
        member(
            format!(
                r"{JAVA_MODS}(?:<[^>\n]+>[ \t]+)?(?:[\w$][\w$<>\[\],.? ]*?[ \t]+)?(?P<name>[\w$]+)[ \t]*\("
            ),
            Function,
        ),
    ])
});

const JS: &str = r"^[ \t]*(?:export[ \t]+)?(?:default[ \t]+)?(?:declare[ \t]+)?";

static SCRIPT: LazyLock<Patterns> = LazyLock::new(|| {
    Patterns::new(vec![
        pattern(
            format!(r"{JS}(?:async[ \t]+)?function[ \t]*\*?[ \t]*(?P<name>[\w$]+)"),
            Function,
            Body,
        ),
        pattern(
            format!(r"{JS}(?:abstract[ \t]+)?class[ \t]+(?P<name>[\w$]+)"),
            Class,
            Type,
        ),
        pattern(format!(r"{JS}interface[ \t]+(?P<name>[\w$]+)"), Trait, Type),
        pattern(
            format!(r"{JS}(?:const[ \t]+)?enum[ \t]+(?P<name>[\w$]+)"),
            Enum,
            Type,
        ),
        pattern(
            format!(r"{JS}type[ \t]+(?P<name>[\w$]+)[ \t]*(?:<[^=\n]*>)?[ \t]*="),
            TypeAlias,
            Body,
        ),
        pattern(
            format!(
                r"{JS}(?:const|let|var)[ \t]+(?P<name>[\w$]+)[ \t]*(?::[^=\n]+)?=[ \t]*(?:async[ \t]+)?(?:function\b|(?:\([^)\n]*\)|[\w$]+)[ \t]*(?::[^=\n]+)?=>)"
            ),
            Function,
            Body,
        ),
        member(
            r"^[ \t]*(?:(?:public|private|protected|static|async|readonly|abstract|override|get|set|declare)[ \t]+)*\*?[ \t]*(?P<name>#?[\w$]+)[ \t]*(?:<[^>\n]*>)?[ \t]*\(",
            Function,
        ),
    ])
});

static GO: LazyLock<Patterns> = LazyLock::new(|| {
    Patterns::new(vec![
        pattern(
            r"^func[ \t]*(?:\([ \t]*(?:\w+[ \t]+)?\*?[ \t]*(?P<recv>\w+)(?:\[[^\]\n]*\])?[ \t]*\)[ \t]*)?(?P<name>\w+)",
            Function,
            Body,
        ),
        pattern(
            r"^[ \t]*type[ \t]+(?P<name>\w+)(?:\[[^\]\n]*\])?[ \t]+struct\b",
            Struct,
            Body,
        ),
        pattern(
            r"^[ \t]*type[ \t]+(?P<name>\w+)(?:\[[^\]\n]*\])?[ \t]+interface\b",
            Trait,
            Body,
        ),
        pattern(
            r"^[ \t]*type[ \t]+(?P<name>\w+)(?:\[[^\]\n]*\])?[ \t]+=?[ \t]*[\w*\[(.]",
            TypeAlias,
            Body,
        ),
    ])
});

/// C's `struct`, `class` and `enum` heads, which run to the block they open.
const C_TYPE: &str = r"^[ \t]*(?:typedef[ \t]+)?(?:template[ \t]*<[^\n]*>[ \t]*)?";

static C: LazyLock<Patterns> = LazyLock::new(|| {
    Patterns::new(vec![
        pattern(
            format!(
                r"{C_TYPE}(?:struct|union)[ \t]+(?:\w+[ \t]+)*?(?P<name>\w+)[ \t]*(?:final[ \t]*)?(?::[^;{{]*)?\z"
            ),
            Struct,
            Type,
        ),
        pattern(
            format!(
                r"{C_TYPE}class[ \t]+(?:\w+[ \t]+)*?(?P<name>\w+)[ \t]*(?:final[ \t]*)?(?::[^;{{]*)?\z"
            ),
            Class,
            Type,
        ),
        pattern(
            format!(
                r"{C_TYPE}enum(?:[ \t]+(?:class|struct))?[ \t]+(?P<name>\w+)[ \t]*(?::[^;{{]*)?\z"
            ),
            Enum,
            Type,
        ),
        pattern(
            r"^[ \t]*(?:inline[ \t]+)?namespace\b",
            SymbolKind::Module,
            Scope::Module,
        ),
        pattern(r"^[ \t]*extern\b", SymbolKind::Module, Scope::Module),
        pattern(r"^[ \t]*using[ \t]+(?P<name>\w+)[ \t]*=", TypeAlias, Body),
        pattern(
            r"(?P<name>~?[A-Za-z_]\w*(?:[ \t]*::[ \t]*~?[A-Za-z_]\w*)*|operator[ \t]*[^\s(]+)[ \t\n]*\((?:[^()]|\([^()]*\))*\)[^;(){}=]*\z",
            Function,
            Body,
        ),
    ])
});

/// Words a call or a statement starts with, which no declaration is named.
const NOT_NAMES: &[&str] = &[
    "if",
    "for",
    "while",
    "switch",
    "catch",
    "return",
    "throw",
    "else",
    "try",
    "do",
    "super",
    "this",
    "sizeof",
    "typeof",
    "await",
    "yield",
    "function",
    "synchronized",
    "when",
    "defined",
    "delete",
    "case",
    "assert",
];

/// The last declaration a language's patterns name in the statement `range` of `masked`, read
/// inside the block `top`. A member pattern counts only directly inside a type.
fn find_decl(lang: Lang, masked: &str, range: Range<usize>, top: &Open) -> Option<Found> {
    let patterns: &Patterns = match lang {
        Lang::Rust => &RUST,
        Lang::Kotlin => &KOTLIN,
        Lang::Java => &JAVA,
        Lang::Script => &SCRIPT,
        Lang::Go => &GO,
        Lang::C => &C,
        Lang::Python => return None,
    };
    let head = &masked[range.clone()];
    let mut best: Option<Found> = None;
    for i in patterns.set.matches(head).iter() {
        let (re, p) = &patterns.each[i];
        if p.member && top.scope != Scope::Type {
            continue;
        }
        let Some(c) = re.captures_iter(head).last() else {
            continue;
        };
        let whole = c.get(0).expect("the whole match");
        let start = range.start + whole.start();
        if best.as_ref().is_some_and(|b| b.at >= start) {
            continue;
        }
        let mut name = c.name("name").map_or("", |m| m.as_str()).to_string();
        // C's `Foo :: bar` is `bar`, of `Foo`; Kotlin's `` `a test` `` is `a test`.
        let qualified = (lang == Lang::C && name.contains("::")).then(|| {
            name.retain(|c| !c.is_whitespace());
            name.clone()
        });
        if lang == Lang::Kotlin {
            name = name.trim_matches('`').to_string();
        }
        // Only a pattern without a keyword of its own can take a call or a statement for one.
        let bare = p.member || lang == Lang::C && p.kind == Function;
        if name == "_"
            || bare && NOT_NAMES.contains(&name.as_str())
            || lang == Lang::C && p.kind == Function && top.scope == Scope::Body
        {
            continue;
        }
        let mut container = c.name("recv").map(|m| m.as_str().to_string());
        if let Some((outer, last)) = qualified.as_deref().and_then(|q| q.rsplit_once("::")) {
            container = outer.rsplit("::").next().map(str::to_string);
            name = last.to_string();
        }
        let at = start + whole.as_str().len() - whole.as_str().trim_start().len();
        let mut line = masked[..at].rfind('\n').map_or(0, |n| n + 1);
        // A Kotlin or Java declaration starts at its annotations, as its grammar has it.
        while matches!(lang, Lang::Kotlin | Lang::Java) && line > range.start {
            let above = masked[..line - 1]
                .rfind('\n')
                .map_or(0, |n| n + 1)
                .max(range.start);
            match masked[above..line].trim_start().starts_with('@') {
                true => line = above,
                false => break,
            }
        }
        best = Some(Found {
            kind: p.kind,
            name,
            container,
            line,
            at,
            scope: p.scope,
        });
    }
    best
}

/// The type a nameless [`Scope::Type`] block belongs to: for a Rust `impl` (`head` from its
/// keyword on) the type after its `for`, or its first; for a Kotlin companion object, the class
/// around it.
fn scope_type(lang: Lang, head: &str, stack: &[Open]) -> String {
    match lang {
        Lang::Rust => impl_type(&head[head.find("impl").map_or(0, |i| i + 4)..]),
        _ => stack
            .iter()
            .rev()
            .find_map(|o| o.name.clone())
            .unwrap_or_else(|| "Companion".to_string()),
    }
}

/// The type an `impl` header (what follows `impl`) is for: `<T> Trait<T> for a::Type<T>` is
/// `Type`.
fn impl_type(head: &str) -> String {
    let plain = strip_generics(head);
    let ty = match plain.split_once(" for ") {
        Some((_, ty)) => ty,
        None => &plain,
    };
    let ty = ty
        .split_whitespace()
        .find(|w| !matches!(*w, "dyn" | "&" | "mut" | "&mut" | "!" | "unsafe"))
        .unwrap_or_default();
    let ty = ty.trim_start_matches(['&', '*', '!']);
    let ty = ty.split(['<', '(', '{']).next().unwrap_or_default();
    ty.rsplit("::").next().unwrap_or_default().to_string()
}

/// `text` without what stands between angle brackets, an arrow's `>` not closing one.
fn strip_generics(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let mut last = ' ';
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' if last != '-' && depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
        last = c;
    }
    out
}

/// C's `#define`s, which a statement-at-a-time reading never sees.
fn defines(masked: &str, text: &str) -> Vec<Decl> {
    static DEFINE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?m)^[ \t]*#[ \t]*define[ \t]+(\w+)").expect("a valid regex")
    });
    DEFINE
        .captures_iter(masked)
        .map(|c| {
            let whole = c.get(0).expect("the whole match");
            let end = text[whole.end()..]
                .find('\n')
                .map_or(text.len(), |n| whole.end() + n);
            Decl {
                kind: Macro,
                name: c[1].to_string(),
                container: None,
                range: whole.start()..end,
                line: 0,
                end_line: 0,
                signature: text[whole.start()..end].trim().chars().take(200).collect(),
                test: false,
            }
        })
        .collect()
}

// ------------------------------------------------------------------------------------- Python

/// Python's `def`s and `class`es, each ending where a line is indented no deeper than it.
fn indented(masked: &str, text: &str) -> Vec<Decl> {
    static DECL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?m)^([ \t]*)(?:async[ \t]+)?(def|class)[ \t]+(\w+)").expect("a valid regex")
    });
    let starts = line_starts(masked);
    let lines: Vec<&str> = masked.split('\n').collect();
    let indent = |l: &str| l.len() - l.trim_start().len();
    let mut out: Vec<Decl> = Vec::new();
    // The classes and functions around the one being read: indent, a class's name, a test's.
    let mut around: Vec<(usize, usize, Option<String>, bool)> = Vec::new();
    for c in DECL.captures_iter(masked) {
        let whole = c.get(0).expect("the whole match");
        let depth = c[1].len();
        let is_class = &c[2] == "class";
        let name = c[3].to_string();
        // The header runs to the `:` outside brackets, over lines if its arguments do.
        let mut brackets = 0usize;
        let mut colon = masked.len();
        for (j, ch) in masked[whole.end()..].bytes().enumerate() {
            match ch {
                b'(' | b'[' | b'{' => brackets += 1,
                b')' | b']' | b'}' => brackets = brackets.saturating_sub(1),
                b':' if brackets == 0 => {
                    colon = whole.end() + j;
                    break;
                }
                _ => {}
            }
        }
        let first = starts.partition_point(|&s| s <= colon);
        let mut last = first;
        for (n, line) in lines.iter().enumerate().skip(first) {
            if line.trim().is_empty() {
                continue;
            }
            if indent(line) <= depth {
                break;
            }
            last = n + 1;
        }
        let end = (starts[last - 1] + lines[last - 1].len()).min(text.len());
        around.retain(|&(_, until, _, _)| until > whole.start());
        let container = around.last().and_then(|a| a.2.clone());
        let in_test = around.last().is_some_and(|a| a.3);
        let test = in_test || name.starts_with("test") || is_class && name.starts_with("Test");
        let kind = match (is_class, &container) {
            (true, _) => Class,
            (false, Some(_)) if around.last().is_some_and(|a| a.0 < depth) => Method,
            _ => Function,
        };
        around.push((depth, end, is_class.then(|| name.clone()), test));
        let signature = text[whole.start()..colon.min(text.len())]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        out.push(Decl {
            kind,
            name,
            container: if kind == Method { container } else { None },
            range: whole.start()..end,
            line: 0,
            end_line: 0,
            signature: signature.chars().take(200).collect(),
            test,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a test compares: kind, name, container, first and last line, test.
    type Row = (SymbolKind, String, Option<String>, u32, u32, bool);

    fn rows(lang: Lang, text: &str) -> Vec<Row> {
        symbols(lang, text)
            .into_iter()
            .map(|d| (d.kind, d.name, d.container, d.line, d.end_line, d.test))
            .collect()
    }

    fn row(kind: SymbolKind, name: &str, container: Option<&str>, lines: (u32, u32)) -> Row {
        (
            kind,
            name.into(),
            container.map(Into::into),
            lines.0,
            lines.1,
            false,
        )
    }

    fn test(kind: SymbolKind, name: &str, container: Option<&str>, lines: (u32, u32)) -> Row {
        (
            kind,
            name.into(),
            container.map(Into::into),
            lines.0,
            lines.1,
            true,
        )
    }

    /// Every number below counts the lines of its fixture from 1.
    #[test]
    fn rust_items_and_their_containers() {
        let text = r##"//! A module { with braces in a comment
use std::fmt;

/// A doc { comment
#[derive(Debug)]
pub struct Point {
    x: i32,
}

pub(crate) struct Unit;
struct Tuple(u8, [u8; 4]);

pub enum Shape { Circle { r: f64 }, Square }

pub trait Area {
    fn area(&self) -> f64;
    fn twice(&self) -> f64 {
        self.area() * 2.0
    }
}

impl<T: Fn() -> u8> Area for Wrapper<T> where T: Clone {
    type Output = u8;
    const SIDES: u8 = 4;
    fn area(&self) -> f64 {
        let s = "}{ not code";
        let c = '{';
        let r = r#"fn fake() {"#;
        let l: &'static str = "x";
        0.0
    }
}

impl Point {
    pub const fn new(x: i32) -> Self {
        fn helper() {}
        Point { x }
    }
    async unsafe fn spin<'a>(&'a self) {}
}

pub type Pair<'a> = (&'a str, &'a str);
static NAMES: [&str; 2] = ["a", "b"];
macro_rules! square { ($x:expr) => { $x * $x }; }
extern "C" fn callback() {}
fn r#match() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {}
    #[tokio::test]
    async fn it_waits() {}
}
"##;
        assert_eq!(
            rows(Lang::Rust, text),
            [
                row(Struct, "Point", None, (6, 8)),
                row(Struct, "Unit", None, (10, 10)),
                row(Struct, "Tuple", None, (11, 11)),
                row(Enum, "Shape", None, (13, 13)),
                row(Trait, "Area", None, (15, 20)),
                row(Method, "area", Some("Area"), (16, 16)),
                row(Method, "twice", Some("Area"), (17, 19)),
                row(TypeAlias, "Output", Some("Wrapper"), (23, 23)),
                row(Const, "SIDES", Some("Wrapper"), (24, 24)),
                row(Method, "area", Some("Wrapper"), (25, 31)),
                row(Method, "new", Some("Point"), (35, 38)),
                row(Function, "helper", None, (36, 36)),
                row(Method, "spin", Some("Point"), (39, 39)),
                row(TypeAlias, "Pair", None, (42, 42)),
                row(Const, "NAMES", None, (43, 43)),
                row(Macro, "square", None, (44, 44)),
                row(Function, "callback", None, (45, 45)),
                row(Function, "r#match", None, (46, 46)),
                test(SymbolKind::Module, "tests", None, (49, 56)),
                test(Function, "it_works", None, (53, 53)),
                test(Function, "it_waits", None, (55, 55)),
            ]
        );
        let new = &symbols(Lang::Rust, text)[10];
        assert_eq!(new.signature, "pub const fn new(x: i32) -> Self");
        assert!(text[new.range.clone()].starts_with("    pub const fn new"));
        assert!(text[new.range.clone()].ends_with("Point { x }\n    }"));
    }

    /// The scanner on files of this repository, whose every `fn` line it has to find, the
    /// strings, raw strings, lifetimes and doc comments in them notwithstanding.
    #[test]
    fn rust_files_of_this_repository() {
        let fns = Regex::new(
            r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?(?:(?:const|async|unsafe)[ \t]+)*fn[ \t]+(\w+)",
        )
        .unwrap();
        for text in [
            include_str!("index/links.rs"),
            include_str!("index/search.rs"),
            include_str!("markdown/mod.rs"),
            include_str!("code.rs"),
        ] {
            let found = symbols(Lang::Rust, text);
            let callables: Vec<&str> = found
                .iter()
                .filter(|d| d.kind.callable() && d.kind != Macro)
                .map(|d| d.name.as_str())
                .collect();
            let masked = mask(Lang::Rust, text);
            let lines: Vec<&str> = fns
                .captures_iter(&masked)
                .map(|c| c.get(1).unwrap().as_str())
                .collect();
            assert_eq!(callables, lines);
            for d in &found {
                let line = text.lines().nth(d.line as usize - 1).unwrap();
                assert!(line.contains(d.name.as_str()), "{d:?} on {line:?}");
            }
        }
        let links = symbols(Lang::Rust, include_str!("index/links.rs"));
        let find = |name: &str| links.iter().find(|d| d.name == name).unwrap();
        assert_eq!(find("resolve_links_of").kind, Function);
        assert_eq!(find("backlinks").container.as_deref(), Some("Index"));
        assert!(find("links_from_lists_where_each_link_leads").test);
        assert!(!find("links_from").test);
    }

    #[test]
    fn python_by_indentation() {
        let text = r#""""Module docstring with def fake(): and class Fake:"""
import os

CONST = "def not_a_function():"

def top(a,
        b):
    """Doc."""
    return a  # def comment():

class Thing(Base):
    attr = 1

    def method(self):
        def inner():
            pass
        return inner

    async def other(self) -> None: ...

    @property
    def prop(self):
        return 1

def test_thing():
    assert Thing()

class TestGroup:
    def check(self):
        pass
"#;
        assert_eq!(
            rows(Lang::Python, text),
            [
                row(Function, "top", None, (6, 9)),
                row(Class, "Thing", None, (11, 23)),
                row(Method, "method", Some("Thing"), (14, 17)),
                row(Function, "inner", None, (15, 16)),
                row(Method, "other", Some("Thing"), (19, 19)),
                row(Method, "prop", Some("Thing"), (22, 23)),
                test(Function, "test_thing", None, (25, 26)),
                test(Class, "TestGroup", None, (28, 30)),
                test(Method, "check", Some("TestGroup"), (29, 30)),
            ]
        );
        // As codegraph's tree-sitter parse has them.
        let xtest = symbols(Lang::Python, include_str!("../../../build-aux/xtest.py"));
        let lines: Vec<(&str, u32, u32)> = xtest
            .iter()
            .map(|d| (d.name.as_str(), d.line, d.end_line))
            .collect();
        assert_eq!(
            lines,
            [
                ("move", 22, 23),
                ("keycode", 24, 28),
                ("key", 29, 33),
                ("focus", 34, 42),
                ("button", 43, 44)
            ]
        );
    }

    /// A declaration starts at its annotations, as Kotlin's and Java's grammars have it.
    #[test]
    fn kotlin_classes_objects_and_expression_bodies() {
        let text = r#"package a.b

import x.y

/* class Fake { */
data class Point(val x: Int, val y: Int)

interface Shape {
    fun area(): Double
    fun name() = "shape {"
}

class Circle(private val r: Double) : Shape {
    override fun area(): Double {
        val s = "}"
        return 3.14 * r * r
    }

    companion object {
        fun unit() = Circle(1.0)
    }
}

enum class Color { RED, GREEN }

object Registry {
    fun register(s: Shape) {}
}

fun <T> List<T>.second(): T = this[1]

typealias Shapes = List<Shape>

class CircleTest {
    @Test
    fun `area is pi r squared`() {
        assertEquals(3.14, Circle(1.0).area(), 0.01)
    }
}
"#;
        assert_eq!(
            rows(Lang::Kotlin, text),
            [
                row(Class, "Point", None, (6, 6)),
                row(Trait, "Shape", None, (8, 11)),
                row(Method, "area", Some("Shape"), (9, 9)),
                row(Method, "name", Some("Shape"), (10, 10)),
                row(Class, "Circle", None, (13, 22)),
                row(Method, "area", Some("Circle"), (14, 17)),
                row(Method, "unit", Some("Circle"), (20, 20)),
                row(Enum, "Color", None, (24, 24)),
                row(Class, "Registry", None, (26, 28)),
                row(Method, "register", Some("Registry"), (27, 27)),
                row(Function, "second", None, (30, 30)),
                row(TypeAlias, "Shapes", None, (32, 32)),
                row(Class, "CircleTest", None, (34, 39)),
                test(Method, "area is pi r squared", Some("CircleTest"), (35, 38)),
            ]
        );
    }

    #[test]
    fn java_members_but_not_calls_or_enum_constants() {
        let text = r#"package a;

import java.util.List;

/** Doc { */
public class Shapes {
    private final List<String> names = List.of("a{");

    public Shapes() {
        super();
    }

    public static <T> T first(List<T> list) {
        return list.get(0);
    }

    @Override
    public String toString() {
        return "Shapes";
    }

    interface Visitor {
        void visit(Shapes s);
    }

    enum Kind {
        ROUND("r"), SQUARE("s");
        Kind(String code) {}
    }

    abstract static class Base {
        abstract int size();
    }
}
"#;
        assert_eq!(
            rows(Lang::Java, text),
            [
                row(Class, "Shapes", None, (6, 34)),
                row(Method, "Shapes", Some("Shapes"), (9, 11)),
                row(Method, "first", Some("Shapes"), (13, 15)),
                row(Method, "toString", Some("Shapes"), (17, 20)),
                row(Trait, "Visitor", Some("Shapes"), (22, 24)),
                row(Method, "visit", Some("Visitor"), (23, 23)),
                row(Enum, "Kind", Some("Shapes"), (26, 29)),
                row(Method, "Kind", Some("Kind"), (28, 28)),
                row(Class, "Base", Some("Shapes"), (31, 33)),
                row(Method, "size", Some("Base"), (32, 32)),
            ]
        );
    }

    #[test]
    fn script_functions_classes_and_literals() {
        let text = r#"// function fake() {
import { x } from "y";

export function add(a: number, b: number): number {
  return a + b;
}

export default async function* gen() {}

const re = /\{[}]/g;
const tpl = `${a} { }`;

export const mul = (a: number, b: number) => {
  return a * b;
};

export class Box<T> extends Base {
  #secret = 1;
  constructor(private v: T) {
    super();
  }
  get value(): T {
    return this.v;
  }
  static of<T>(v: T): Box<T> {
    if (v) {
      return new Box(v);
    }
  }
}

export interface Shape {
  area(): number;
}

export type Pair = [number, number];

enum Color { Red, Green }

describe("Box", () => {
  it("holds", () => {});
});
"#;
        assert_eq!(
            rows(Lang::Script, text),
            [
                row(Function, "add", None, (4, 6)),
                row(Function, "gen", None, (8, 8)),
                row(Function, "mul", None, (13, 15)),
                row(Class, "Box", None, (17, 30)),
                row(Method, "constructor", Some("Box"), (19, 21)),
                row(Method, "value", Some("Box"), (22, 24)),
                row(Method, "of", Some("Box"), (25, 29)),
                row(Trait, "Shape", None, (32, 34)),
                row(Method, "area", Some("Shape"), (33, 33)),
                row(TypeAlias, "Pair", None, (36, 36)),
                row(Enum, "Color", None, (38, 38)),
            ]
        );
        let minified = format!("function a(){{}}{}", " ".repeat(MINIFIED));
        assert!(symbols(Lang::Script, &minified).is_empty());
    }

    #[test]
    fn go_types_functions_and_receivers() {
        let text = r#"package main

import "fmt"

// func fake() {
type Point struct {
	X, Y int
}

type Shape interface {
	Area() float64
}

type ID int

type Alias = Point

func (p *Point) Area() float64 {
	s := "}"
	r := '{'
	raw := `{`
	return 0
}

func main() {
	fmt.Println("hi")
}

func TestMain(t *testing.T) {}
"#;
        assert_eq!(
            rows(Lang::Go, text),
            [
                row(Struct, "Point", None, (6, 8)),
                row(Trait, "Shape", None, (10, 12)),
                row(TypeAlias, "ID", None, (14, 14)),
                row(TypeAlias, "Alias", None, (16, 16)),
                row(Method, "Area", Some("Point"), (18, 23)),
                row(Function, "main", None, (25, 27)),
                test(Function, "TestMain", None, (29, 29)),
            ]
        );
    }

    #[test]
    fn c_definitions_not_calls_or_prototypes() {
        let text = r#"#include <stdio.h>
#define MAX(a, b) ((a) > (b) ? (a) : (b))

/* int fake(void) { */
struct point {
    int x, y;
};

typedef struct node {
    struct node *next;
} node_t;

enum color { RED, GREEN };

static int
add(int a, int b)
{
    const char *s = "}";
    char c = '{';
    if (a > b) {
        return a;
    }
    return a + b;
}

int main(void) {
    for (int i = 0; i < 3; i++) {
        printf("%d\n", add(i, 1));
    }
    return 0;
}

namespace geo {
class Shape : public Base {
public:
    virtual double area() const = 0;
    double twice() const { return 2 * area(); }
};
}

double geo::Shape::half() const {
    return area() / 2;
}
"#;
        assert_eq!(
            rows(Lang::C, text),
            [
                row(Macro, "MAX", None, (2, 2)),
                row(Struct, "point", None, (5, 7)),
                row(Struct, "node", None, (9, 11)),
                row(Enum, "color", None, (13, 13)),
                row(Function, "add", None, (16, 24)),
                row(Function, "main", None, (26, 31)),
                row(Class, "Shape", None, (34, 38)),
                row(Method, "twice", Some("Shape"), (37, 37)),
                row(Method, "half", Some("Shape"), (41, 43)),
            ]
        );
    }

    #[test]
    fn calls_are_names_before_a_parenthesis() {
        let body = mask(
            Lang::Rust,
            "{ let x = foo(1); bar::<u8>(x); vec![]; println!(\"baz(\"); foo(2) }",
        );
        assert_eq!(calls(&body), ["foo", "bar", "println"]);
    }

    #[test]
    fn languages_and_test_files_by_path() {
        assert_eq!(lang_of("src/main.rs"), Some(Lang::Rust));
        assert_eq!(lang_of("app/x.TSX"), Some(Lang::Script));
        assert_eq!(lang_of("notes/a.md"), None);
        assert_eq!(lang_of("Makefile"), None);
        assert!(is_test_path("apps/cli/tests/mcp.rs"));
        assert!(is_test_path("crates/core/src/git/tests.rs"));
        assert!(is_test_path("pkg/x_test.go"));
        assert!(is_test_path("tests/test_api.py"));
        assert!(is_test_path("src/box.spec.ts"));
        assert!(!is_test_path("crates/core/src/index/links.rs"));
    }
}
