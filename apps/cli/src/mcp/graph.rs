//! Call relationships for explore, using indexed declarations and names or language-server call
//! hierarchy (`precise.rs`). Name matches do not resolve individual declarations; ambiguous
//! names are reported.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use accent_api::{CodeSymbol, Mention, Read};
use accent_core::code;

use super::explore::count;
use super::precise::Precise;
use super::{Shared, fail};

/// Maximum declarations per name and names followed.
const DEFS: usize = 3;
const SEEDS: usize = 6;
/// Maximum references to one name.
const MENTIONS: usize = 200;
/// Caller depth and fan-out when searching for tests.
const HOPS: usize = 3;
const PER_HOP: usize = 8;
/// Call-path depth and total declarations traversed.
const PATH_HOPS: usize = 3;
const PATH_NODES: usize = 150;

/// A declaration named in the question.
pub(super) struct Seed {
    pub sym: CodeSymbol,
    /// Number of declarations sharing this name.
    pub defs: usize,
}

/// Resolve `names` to declarations, matching bare or qualified names in the given case.
pub(super) fn seeds(s: &Shared, names: &[String]) -> Result<Vec<Seed>, String> {
    let mut out: Vec<Seed> = Vec::new();
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
        // Every declaration of the name counts, whatever its container: a call reaches any.
        let defs = s
            .vault
            .symbols_named(&[bare.to_string()], "", 50)
            .map_err(fail)?
            .len();
        for sym in exact.into_iter().take(DEFS) {
            if !out.iter().any(|o| same(&o.sym, &sym)) {
                out.push(Seed { sym, defs });
            }
        }
    }
    out.truncate(SEEDS);
    Ok(out)
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
            false => "**Blast radius** (by name: a call counts for every declaration of its name)",
        }
        .to_string(),
    ];
    for seed in seeds {
        let sym = &seed.sym;
        let (callers, by_name) = edges.callers(sym)?;
        let mut by_file: BTreeMap<&str, usize> = BTreeMap::new();
        for m in &callers {
            *by_file.entry(m.rel_path.as_str()).or_default() += 1;
        }
        let files: Vec<String> = by_file.keys().take(6).map(|f| format!("`{f}`")).collect();
        let more = by_file.len().saturating_sub(files.len());
        let mut line = format!(
            "- `{}` {} ({}:{}) — ",
            label(sym),
            sym.kind.label(),
            sym.rel_path,
            sym.line
        );
        line.push_str(&match callers.len() {
            0 => "no callers found".to_string(),
            n => format!(
                "{} in {}{}",
                count(n, "call"),
                files.join(", "),
                match more {
                    0 => String::new(),
                    more => format!(" +{}", count(more, "file")),
                }
            ),
        });
        line.push_str(&format!("; {}", tested(&mut edges, sym, &callers)?));
        if by_name && precise {
            line.push_str("; by name");
        }
        if by_name && seed.defs > 1 {
            line.push_str(&format!("; {} declarations share this name", seed.defs));
        }
        lines.push(line);
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
    mentioned: HashMap<String, Vec<Mention>>,
    precise: Option<Precise<'a>>,
}

impl Edges<'_> {
    /// Return callers of `sym`, their containing declarations, and whether matching used names.
    fn callers(&mut self, sym: &CodeSymbol) -> Result<(Vec<Mention>, bool), String> {
        if let Some(found) = self.precise.as_mut().and_then(|p| p.callers(sym)) {
            return Ok((found, false));
        }
        Ok((
            callers(self.s, &mut self.mentioned, &sym.name)?.clone(),
            true,
        ))
    }

    fn callees(&mut self, sym: &CodeSymbol) -> Result<Vec<CodeSymbol>, String> {
        if let Some(found) = self.precise.as_mut().and_then(|p| p.callees(sym)) {
            return Ok(found);
        }
        self.code.callees(sym)
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

/// Find references to `name` within other declarations, reading each file once.
fn callers<'a>(
    s: &Shared,
    mentioned: &'a mut HashMap<String, Vec<Mention>>,
    name: &str,
) -> Result<&'a Vec<Mention>, String> {
    if !mentioned.contains_key(name) {
        let found = s.vault.mentions(name, MENTIONS).map_err(fail)?;
        let found = found
            .into_iter()
            .filter(|m| m.symbol.is_some() && s.shown(&m.rel_path))
            .collect();
        mentioned.insert(name.to_string(), found);
    }
    Ok(&mentioned[name])
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
                    let mut path = vec![to.sym.name.clone()];
                    let mut step = Some(at.clone());
                    while let Some(c) = step {
                        path.push(c.name.clone());
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
        let found = s
            .vault
            .symbols_named(&names, &sym.rel_path, DEFS)
            .map_err(fail)?;
        Ok(found
            .into_iter()
            .filter(|c| c.kind.callable() && s.shown(&c.rel_path))
            .collect())
    }
}
