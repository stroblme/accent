//! Call relationships for explore, using indexed declarations and names or language-server call
//! hierarchy and implementations (`precise.rs`). Name matches do not resolve individual
//! declarations; ambiguous names are reported. By name, a callable is used where its name is
//! called in its own language, and a type wherever its name is written there; a bare call in a
//! file that declares a function of that name is that one's.

use std::collections::{HashMap, HashSet, VecDeque};

use accent_api::{CodeSymbol, Mention, Read};
use accent_core::code::{self, Lang};

use super::explore::count;
use super::precise::Precise;
use super::{Shared, fail};

/// Maximum declarations per name and names followed.
const DEFS: usize = 3;
const SEEDS: usize = 6;
/// Maximum references to one name.
const MENTIONS: usize = 200;
/// Call sites quoted for a seed, and the characters of each line quoted.
const SITES: usize = 3;
const SITE_TEXT: usize = 120;
/// Subtypes listed for a seed.
const SUBTYPES: usize = 6;
/// Caller depth and fan-out when searching for tests.
const HOPS: usize = 3;
const PER_HOP: usize = 8;
/// Call-path depth and total declarations traversed.
const PATH_HOPS: usize = 3;
const PATH_NODES: usize = 150;

/// A type implementing or extending another, with the file and line that say so.
pub(super) type Subtype = (String, String, u32);

/// A declaration named in the question.
pub(super) struct Seed {
    pub sym: CodeSymbol,
    /// Number of declarations sharing this name.
    pub defs: usize,
}

/// Resolve `names` to declarations, matching bare or qualified names in the given case; the names
/// nothing is declared by as well.
pub(super) fn seeds(s: &Shared, names: &[String]) -> Result<(Vec<Seed>, Vec<String>), String> {
    let mut out: Vec<Seed> = Vec::new();
    let mut missing = Vec::new();
    for name in names {
        let bare = name.rsplit([':', '.']).next().unwrap_or(name);
        let found = s.vault.find_symbols(name, 50).map_err(fail)?;
        let exact: Vec<CodeSymbol> = found
            .into_iter()
            .filter(|c| c.name.eq_ignore_ascii_case(bare) && s.shown(&c.rel_path))
            .collect();
        let cased = exact.iter().any(|c| c.name == bare);
        let exact: Vec<CodeSymbol> = exact
            .into_iter()
            .filter(|c| !cased || c.name == bare)
            .collect();
        if exact.is_empty() {
            missing.push(name.clone());
        }
        // Every declaration of the name in its language counts, whatever its container: a call
        // reaches any.
        let all = s
            .vault
            .symbols_named(&[bare.to_string()], "", 50)
            .map_err(fail)?;
        for sym in exact.into_iter().take(DEFS) {
            if !out.iter().any(|o| same(&o.sym, &sym)) {
                let defs = all.iter().filter(|d| same_lang(d, &sym)).count();
                out.push(Seed { sym, defs });
            }
        }
    }
    out.truncate(SEEDS);
    Ok((out, missing))
}

/// What explore says about the seeds above its cards, and the declarations it shows beside them.
#[derive(Default)]
pub(super) struct Graph {
    pub summary: String,
    /// Callers and callees of a seed in a file that has a card.
    pub glue: Vec<CodeSymbol>,
}

/// The seeds' callers by file, whether a test reaches them, what they call, and the call paths
/// among them; `shown` the files that get cards, whose callers and callees join the seeds there;
/// `precise` to ask the language servers rather than go by name.
pub(super) fn graph(
    s: &Shared,
    seeds: &[Seed],
    shown: &[String],
    precise: bool,
) -> Result<Graph, String> {
    let mut g = Graph::default();
    if seeds.is_empty() {
        return Ok(g);
    }
    let mut edges = Edges {
        s,
        code: Reader::new(s),
        mentioned: HashMap::new(),
        precise: precise.then(|| Precise::new(s)),
    };
    let mut lines = vec![
        match precise {
            true => "**Blast radius** (precise: by the language servers' call hierarchy)",
            false => {
                "**Blast radius** (by name: a call counts for every declaration of its name in its \
                 language)"
            }
        }
        .to_string(),
    ];
    for seed in seeds {
        let sym = &seed.sym;
        let (callers, by_name) = edges.callers(sym)?;
        // A type, a const or a trait is named rather than called.
        let word = match sym.kind.callable() {
            true => "call",
            false => "reference",
        };
        let files: HashSet<&str> = callers.iter().map(|m| m.rel_path.as_str()).collect();
        let mut line = format!(
            "- `{}` {} ({}:{}) — ",
            label(sym),
            sym.kind.label(),
            sym.rel_path,
            sym.line
        );
        line.push_str(&match (callers.len(), files.len()) {
            (0, _) => format!("no {word}s found"),
            (n, 1) => count(n, word),
            (n, f) => format!("{} in {}", count(n, word), count(f, "file")),
        });
        line.push_str(&format!("; {}", tested(&mut edges, sym, &callers)?));
        if by_name && precise {
            line.push_str("; by name");
        }
        if by_name && seed.defs > 1 {
            line.push_str(&format!("; {} declarations share this name", seed.defs));
        }
        lines.push(line);
        lines.extend(sites(&callers));
        if !sym.kind.callable() {
            let (found, by_name) = edges.subtypes(sym)?;
            lines.extend(subtypes(&found, by_name && precise));
        }
        let callees = edges.callees(sym)?;
        let mut called: Vec<String> = Vec::new();
        for c in &callees {
            if !called.contains(&c.name) {
                called.push(c.name.clone());
            }
        }
        if !called.is_empty() {
            let more = called.len().saturating_sub(10);
            called.truncate(10);
            let more = if more > 0 {
                format!(" +{more}")
            } else {
                String::new()
            };
            lines.push(format!("  calls `{}`{more}", called.join("`, `")));
        }
        // The callers and callees in the files that have cards are shown there with it.
        let near = callers
            .iter()
            .filter_map(|m| m.symbol.clone())
            .chain(callees)
            .filter(|c| shown.contains(&c.rel_path));
        for c in near {
            if !g.glue.iter().any(|o| same(o, &c)) && !seeds.iter().any(|o| same(&o.sym, &c)) {
                g.glue.push(c);
            }
        }
    }
    let paths = call_paths(&mut edges, seeds)?;
    if !paths.is_empty() {
        lines.push(String::new());
        lines.push("**Call paths**".to_string());
        lines.extend(paths.into_iter().map(|p| format!("- {p}")));
    }
    let failed = edges.precise.map(|p| p.failures()).unwrap_or_default();
    for (i, why) in failed.into_iter().enumerate() {
        lines.insert(1 + i, format!("precise: {why}, name-based callers shown"));
    }
    g.summary = lines.join("\n") + "\n\n";
    Ok(g)
}

/// Use language-server call hierarchy when `precise` succeeds; otherwise match by name.
struct Edges<'a> {
    s: &'a Shared,
    code: Reader<'a>,
    mentioned: Mentioned,
    precise: Option<Precise<'a>>,
}

/// The uses of a name found once per exploration, by name, language and whether as calls.
type Mentioned = HashMap<(String, Lang, bool), Vec<Mention>>;

impl Edges<'_> {
    /// Return callers of `sym`, their containing declarations, and whether matching used names.
    fn callers(&mut self, sym: &CodeSymbol) -> Result<(Vec<Mention>, bool), String> {
        if let Some(found) = self.precise.as_mut().and_then(|p| p.callers(sym)) {
            return Ok((found, false));
        }
        Ok((callers(self.s, &mut self.mentioned, sym)?, true))
    }

    fn callees(&mut self, sym: &CodeSymbol) -> Result<Vec<CodeSymbol>, String> {
        if let Some(found) = self.precise.as_mut().and_then(|p| p.callees(sym)) {
            return Ok(found);
        }
        self.code.callees(sym)
    }

    /// Return what implements or extends the type `sym` in its language, and whether matching
    /// used names.
    fn subtypes(&mut self, sym: &CodeSymbol) -> Result<(Vec<Subtype>, bool), String> {
        if let Some(found) = self.precise.as_mut().and_then(|p| p.subtypes(sym)) {
            return Ok((found, false));
        }
        let s = self.s;
        let found = s.vault.subtypes(&sym.name, 50).map_err(fail)?;
        let found = found
            .into_iter()
            .filter(|c| same_lang(c, sym) && s.shown(&c.rel_path))
            .map(|c| (c.container.unwrap_or_default(), c.rel_path, c.line))
            .collect();
        Ok((found, true))
    }

    /// Identify one declaration; name matching conflates declarations with the same name.
    fn key(&self, sym: &CodeSymbol) -> String {
        match self.precise {
            Some(_) => format!("{}:{}", sym.rel_path, sym.byte_start),
            None => sym.name.clone(),
        }
    }
}

/// Format a declaration as `Container::name`.
pub(super) fn label(sym: &CodeSymbol) -> String {
    match &sym.container {
        Some(c) => format!("{c}::{}", sym.name),
        None => sym.name.clone(),
    }
}

/// Whether two rows identify the same declaration.
fn same(a: &CodeSymbol, b: &CodeSymbol) -> bool {
    a.rel_path == b.rel_path && a.byte_start == b.byte_start
}

/// Whether two declarations are written in the same language.
fn same_lang(a: &CodeSymbol, b: &CodeSymbol) -> bool {
    code::lang_of(&a.rel_path) == code::lang_of(&b.rel_path)
}

/// Find where `sym` is used within other declarations of its language, called where it is a
/// callable, reading each file once. A bare call in a file that declares a free function of the
/// name is that function's, and a call of a parameter of the name is the parameter's.
fn callers(
    s: &Shared,
    mentioned: &mut Mentioned,
    sym: &CodeSymbol,
) -> Result<Vec<Mention>, String> {
    let Some(lang) = code::lang_of(&sym.rel_path) else {
        return Ok(Vec::new());
    };
    let key = (sym.name.clone(), lang, sym.kind.callable());
    if !mentioned.contains_key(&key) {
        let found = s.vault.mentions(&sym.name, lang, key.2, MENTIONS);
        let found = found
            .map_err(fail)?
            .into_iter()
            .filter(|m| m.symbol.is_some() && s.shown(&m.rel_path))
            .collect();
        mentioned.insert(key.clone(), found);
    }
    let namesakes: HashSet<String> = s
        .vault
        .symbols_named(std::slice::from_ref(&sym.name), "", 50)
        .map_err(fail)?
        .into_iter()
        .filter(|d| d.container.is_none() && d.kind.callable() && d.rel_path != sym.rel_path)
        .map(|d| d.rel_path)
        .collect();
    let theirs = |m: &Mention| {
        let param = m
            .symbol
            .as_ref()
            .is_some_and(|c| binds(&c.signature, &sym.name));
        param || namesakes.contains(&m.rel_path) && calls_bare(&m.text, &sym.name)
    };
    Ok(mentioned[&key]
        .iter()
        .filter(|m| !theirs(m))
        .cloned()
        .collect())
}

/// Whether a declaration's `signature` takes a parameter called `name`: `(name: T`, `mut name`,
/// Python's `name=None`.
fn binds(signature: &str, name: &str) -> bool {
    let params = signature.split_once('(').map_or("", |(_, p)| p);
    params.split(',').any(|p| {
        let p = p.trim_start();
        let p = ["mut ", "val ", "var "]
            .iter()
            .fold(p, |p, m| p.trim_start_matches(m));
        p.strip_prefix(name)
            .is_some_and(|rest| rest.trim_start().starts_with([':', '=', ')']))
    })
}

/// Whether `line` calls `name` bare, `name(`, rather than as a method or by a path.
fn calls_bare(line: &str, name: &str) -> bool {
    line.match_indices(name).any(|(i, _)| {
        let before = line[..i].trim_end();
        line[i + name.len()..].trim_start().starts_with('(')
            && !before.ends_with(|c: char| c.is_alphanumeric() || "_.:".contains(c))
    })
}

/// The first [`SITES`] of `callers` as `path:line (caller) line`, one a file before a second in
/// any, and the files the rest are in.
fn sites(callers: &[Mention]) -> Vec<String> {
    let mut seen = HashSet::new();
    let (firsts, others): (Vec<&Mention>, Vec<&Mention>) = callers
        .iter()
        .partition(|m| seen.insert(m.rel_path.as_str()));
    let ordered: Vec<&Mention> = firsts.into_iter().chain(others).collect();
    let (shown, rest) = ordered.split_at(SITES.min(ordered.len()));
    let mut out: Vec<String> = shown
        .iter()
        .map(|m| {
            let caller = m.symbol.as_ref().map(label).unwrap_or_default();
            let mut text: String = m.text.chars().take(SITE_TEXT).collect();
            if text.len() < m.text.len() {
                text.push('…');
            }
            format!("  {}:{} ({caller}) {text}", m.rel_path, m.line)
        })
        .collect();
    if !rest.is_empty() {
        let mut files: Vec<&str> = Vec::new();
        for m in rest {
            if !files.contains(&m.rel_path.as_str()) {
                files.push(&m.rel_path);
            }
        }
        let named: Vec<String> = files.iter().take(5).map(|f| format!("`{f}`")).collect();
        let more = match files.len() - named.len() {
            0 => String::new(),
            n => format!(" +{}", count(n, "file")),
        };
        out.push(format!(
            "  +{} more in {}{more}",
            rest.len(),
            named.join(", ")
        ));
    }
    out
}

/// List the subtypes `found`, each type and where it says so, marked `by_name` where `precise`
/// fell back to the scanner's rows.
fn subtypes(found: &[Subtype], by_name: bool) -> Option<String> {
    if found.is_empty() {
        return None;
    }
    let named: Vec<String> = found
        .iter()
        .take(SUBTYPES)
        .map(|(ty, rel, line)| format!("`{ty}` ({rel}:{line})"))
        .collect();
    let more = match found.len() - named.len() {
        0 => String::new(),
        n => format!(" +{n}"),
    };
    let by_name = if by_name { "; by name" } else { "" };
    Some(format!("  subtypes {}{more}{by_name}", named.join(", ")))
}

/// Whether a test reaches `sym` within [`HOPS`] caller hops.
fn tested(edges: &mut Edges, sym: &CodeSymbol, callers_of: &[Mention]) -> Result<String, String> {
    let is_test = |m: &Mention| m.symbol.as_ref().is_some_and(|c| c.test);
    if let Some(t) = callers_of.iter().find(|m| is_test(m)) {
        let name = t.symbol.as_ref().map_or("", |c| c.name.as_str());
        return Ok(format!("tested by `{name}` ({})", t.rel_path));
    }
    let mut asked: HashSet<String> = HashSet::from([edges.key(sym)]);
    let mut frontier: Vec<CodeSymbol> =
        callers_of.iter().filter_map(|m| m.symbol.clone()).collect();
    for _ in 1..HOPS {
        let mut next = Vec::new();
        let ask: Vec<CodeSymbol> = frontier
            .into_iter()
            .filter(|c| asked.insert(edges.key(c)))
            .take(PER_HOP)
            .collect();
        for c in ask {
            let (found, _) = edges.callers(&c)?;
            if let Some(t) = found.iter().find(|m| is_test(m)) {
                return Ok(format!("tested via `{}` ({})", c.name, t.rel_path));
            }
            next.extend(found.into_iter().filter_map(|m| m.symbol));
        }
        frontier = next;
    }
    Ok(format!("no tests found within {HOPS} caller hops"))
}

/// Format call paths between seeds within [`PATH_HOPS`] hops.
fn call_paths(edges: &mut Edges, seeds: &[Seed]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for from in seeds {
        let mut parent: HashMap<(String, i64), Option<CodeSymbol>> = HashMap::new();
        let key = |c: &CodeSymbol| (c.rel_path.clone(), c.byte_start);
        parent.insert(key(&from.sym), None);
        let mut queue = VecDeque::from([(from.sym.clone(), 0)]);
        while let Some((at, depth)) = queue.pop_front() {
            if depth == PATH_HOPS || parent.len() > PATH_NODES {
                continue;
            }
            for next in edges.callees(&at)? {
                if parent.contains_key(&key(&next)) {
                    continue;
                }
                parent.insert(key(&next), Some(at.clone()));
                if let Some(to) = seeds.iter().find(|s| same(&s.sym, &next)) {
                    let mut path = vec![label(&to.sym)];
                    let mut step = Some(at.clone());
                    while let Some(c) = step {
                        path.push(label(&c));
                        step = parent.get(&key(&c)).cloned().flatten();
                    }
                    path.reverse();
                    out.push(format!("`{}`", path.join("` → `")));
                }
                queue.push_back((next, depth + 1));
            }
        }
    }
    Ok(out)
}

/// Code files read and masked once per exploration.
struct Reader<'a> {
    s: &'a Shared,
    files: HashMap<String, Option<String>>,
}

impl<'a> Reader<'a> {
    fn new(s: &'a Shared) -> Self {
        Reader {
            s,
            files: HashMap::new(),
        }
    }

    /// Find nearby declarations called by name from `sym`.
    fn callees(&mut self, sym: &CodeSymbol) -> Result<Vec<CodeSymbol>, String> {
        let s = self.s;
        let entry = self.files.entry(sym.rel_path.clone()).or_insert_with(|| {
            let lang = code::lang_of(&sym.rel_path)?;
            match s.vault.read_text(&sym.rel_path) {
                Ok(Read::Text(t)) => Some(code::mask(lang, &t.text)),
                _ => None,
            }
        });
        let Some(masked) = entry else {
            return Ok(Vec::new());
        };
        let body = masked
            .get(sym.byte_start as usize..sym.byte_end as usize)
            .unwrap_or_default();
        let names: Vec<String> = code::calls(body)
            .into_iter()
            .filter(|n| *n != sym.name)
            .map(str::to_string)
            .collect();
        // More than are kept of a name, as some may be another language's.
        let found = s
            .vault
            .symbols_named(&names, &sym.rel_path, 4 * DEFS)
            .map_err(fail)?;
        let mut kept: HashMap<String, usize> = HashMap::new();
        Ok(found
            .into_iter()
            .filter(|c| c.kind.callable() && same_lang(c, sym) && s.shown(&c.rel_path))
            .filter(|c| {
                let n = kept.entry(c.name.clone()).or_default();
                *n += 1;
                *n <= DEFS
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    /// A bare call is `name(`; a method's, a path's and a longer name's are not.
    #[test]
    fn a_bare_call_is_neither_a_method_nor_a_path() {
        assert!(super::calls_bare("let n = rank (a) + 1;", "rank"));
        for line in ["x.rank(a)", "Self::rank(a)", "let rank = 1;", "prerank(a)"] {
            assert!(!super::calls_bare(line, "rank"), "{line}");
        }
    }

    /// A parameter is bound in its declaration's signature, in any of the languages read.
    #[test]
    fn a_parameter_of_the_name_is_bound() {
        for signature in [
            "fn eventually(probe: impl Fn() -> bool) -> bool",
            "fn f(a: u8, mut probe: P)",
            "def f(self, probe=None)",
            "fun f(probe: () -> Boolean)",
        ] {
            assert!(super::binds(signature, "probe"), "{signature}");
        }
        assert!(!super::binds("fn f(probes: u8) -> Probe", "probe"));
    }
}
