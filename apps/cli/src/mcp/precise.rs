//! explore's `precise` callers and callees: the call hierarchy of the language server the app
//! runs for a file, asked at a declaration's name and answered back in the index's declarations.
//! A declaration no server answers for is left to the names (`graph.rs`), and why is said once.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use accent_api::{Call, CodeSymbol, Mention};
use accent_core::code::{self, Lang, SymbolKind};

use super::Shared;

/// How long one question waits for a language server that is starting or loading its project,
/// or slow to answer: an agent waits on every second. rust-analyzer first answers on this
/// repository 8–14 s after it starts, so a session's first precise call may go by name, saying
/// so, and the next find it loaded.
pub(super) const COLD: Duration = Duration::from_secs(10);

pub(super) struct Precise<'a> {
    s: &'a Shared,
    /// Why a language's server was given up on in this explore: its declarations go by name.
    failed: HashMap<Lang, String>,
    /// Each file's text and declarations, read once.
    files: Files,
    /// The declarations made of a server's items, which the index has no row for, by file and
    /// start.
    made: HashSet<(String, i64)>,
}

type Files = HashMap<String, Option<(String, Vec<CodeSymbol>)>>;

impl<'a> Precise<'a> {
    pub fn new(s: &'a Shared) -> Self {
        Precise {
            s,
            failed: HashMap::new(),
            files: HashMap::new(),
            made: HashSet::new(),
        }
    }

    /// Why a server was given up on, each once: "rust-analyzer not ready".
    pub fn failures(&self) -> Vec<String> {
        let mut out: Vec<String> = self.failed.values().cloned().collect();
        out.sort();
        out.dedup();
        out
    }

    /// The lines calling `sym`, each with the declaration it is in; `None` to go by name.
    pub fn callers(&mut self, sym: &CodeSymbol) -> Option<Vec<Mention>> {
        let calls = self.ask(sym, true)?;
        let mut out: Vec<Mention> = Vec::new();
        for c in calls {
            let rel = &c.decl.path;
            let Some((text, decls)) = file(&mut self.files, self.s, rel) else {
                continue;
            };
            for line in c.lines.iter().map(|l| l + 1) {
                let Some((symbol, made)) = decl_of(text, decls, &c, line) else {
                    continue;
                };
                let same = |m: &Mention| m.line == line && m.symbol.as_ref() == Some(&symbol);
                if out.iter().any(same) {
                    continue;
                }
                if made {
                    self.made
                        .insert((symbol.rel_path.clone(), symbol.byte_start));
                }
                let quote = text.lines().nth(line as usize - 1).unwrap_or_default();
                out.push(Mention {
                    rel_path: rel.clone(),
                    line,
                    text: quote.trim().chars().take(200).collect(),
                    symbol: Some(symbol),
                });
            }
        }
        // In the order a name's are, which a server need not answer in.
        out.sort_by(|a, b| (&a.rel_path, a.line).cmp(&(&b.rel_path, b.line)));
        Some(out)
    }

    /// The declarations `sym` calls inside the vault; `None` to go by name.
    pub fn callees(&mut self, sym: &CodeSymbol) -> Option<Vec<CodeSymbol>> {
        let calls = self.ask(sym, false)?;
        let mut out: Vec<CodeSymbol> = Vec::new();
        for c in calls {
            let line = c.decl.range.start.line + 1;
            let found = file(&mut self.files, self.s, &c.decl.path)
                .and_then(|(text, decls)| decl_of(text, decls, &c, line));
            let Some((symbol, made)) = found.filter(|(d, _)| !out.contains(d)) else {
                continue;
            };
            if made {
                self.made
                    .insert((symbol.rel_path.clone(), symbol.byte_start));
            }
            out.push(symbol);
        }
        Some(out)
    }

    /// The server's answer about `sym`, asked at its name; `None` to go by name.
    fn ask(&mut self, sym: &CodeSymbol, incoming: bool) -> Option<Vec<Call>> {
        // What a type or a macro is called from is where its name is written.
        if !matches!(sym.kind, SymbolKind::Function | SymbolKind::Method) {
            return None;
        }
        let lang = code::lang_of(&sym.rel_path)?;
        if self.failed.contains_key(&lang) {
            return None;
        }
        let made = self.made.contains(&(sym.rel_path.clone(), sym.byte_start));
        let pos = {
            let (text, _) = file(&mut self.files, self.s, &sym.rel_path)?;
            let span = sym.byte_start as usize..sym.byte_end as usize;
            // A server's own item is asked where it said it is, which a macro's call may be.
            let at = code::name_at(text, span.clone(), &sym.name).or(made.then_some(span.start));
            accent_api::language::pos_of(text, at?)
        };
        let asked = self.s.vault.calls(&sym.rel_path, pos, incoming, COLD);
        match accent_lsp::runtime().block_on(asked) {
            Ok(Some(found)) => Some(found),
            // One the index found is left to its name; one the server made up and cannot find
            // again has no calls to show, its name being a macro's.
            Ok(None) => made.then(Vec::new),
            Err(e) => {
                self.failed.insert(lang, format!("{e:#}"));
                None
            }
        }
    }
}

/// `rel`'s text and declarations, read once, if it may be shown.
fn file<'f>(files: &'f mut Files, s: &Shared, rel: &str) -> Option<&'f (String, Vec<CodeSymbol>)> {
    files
        .entry(rel.to_string())
        .or_insert_with(|| {
            let text = s.text(rel).ok()?.text;
            Some((text, s.vault.file_symbols(rel).ok()?)).filter(|_| s.shown(rel))
        })
        .as_ref()
}

/// The declaration at the far end of `c`: the innermost of the index's `decls` with its name
/// whose lines hold `line` (the call's, or the name's), else one made of the server's item — a
/// function a macro writes, as `methods!` writes the vault's, which the index has no row for —
/// and whether it was made.
fn decl_of(text: &str, decls: &[CodeSymbol], c: &Call, line: u32) -> Option<(CodeSymbol, bool)> {
    let indexed = decls
        .iter()
        .filter(|d| d.name == c.name && d.line <= line && line <= d.end_line)
        .min_by_key(|d| d.end_line - d.line);
    if let Some(d) = indexed {
        return Some((d.clone(), false));
    }
    let range = c.decl.range;
    let start = accent_api::language::byte_of(text, range.start)?;
    let end = accent_api::language::byte_of(text, range.end)?;
    let at = text.lines().nth(range.start.line as usize)?;
    let made = CodeSymbol {
        rel_path: c.decl.path.clone(),
        kind: SymbolKind::Function,
        name: c.name.clone(),
        container: None,
        byte_start: start as i64,
        byte_end: end as i64,
        line: range.start.line + 1,
        end_line: range.end.line + 1,
        signature: at.trim().chars().take(200).collect(),
        test: code::is_test_path(&c.decl.path),
    };
    Some((made, true))
}
