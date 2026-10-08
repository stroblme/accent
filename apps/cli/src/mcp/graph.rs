//! explore's code: the declarations a question names, what calls them and what they call, and
//! the calls leading from one to another — all by name, from the index's declarations and the
//! code's own text, as codegraph's are (`resolvedBy: exact-match`). A call is not resolved to the
//! one declaration it reaches: a name several declarations share says so.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use accent_api::{CodeSymbol, Mention, Read};
use accent_core::code;

use super::explore::count;
use super::{Shared, fail};

/// Declarations one name stands for, at most, and the names followed.
const DEFS: usize = 3;
const SEEDS: usize = 6;
/// Places one name is read at, at most.
const MENTIONS: usize = 200;
/// Caller hops a test is looked for within, and the callers asked at each.
const HOPS: usize = 3;
const PER_HOP: usize = 8;
/// Calls a call path follows from a declaration, and declarations it reads in all.
const PATH_HOPS: usize = 3;
const PATH_NODES: usize = 150;

/// A declaration the question names.
pub(super) struct Seed {
    pub sym: CodeSymbol,
    /// How many declarations share its name.
    pub defs: usize,
}

/// The declarations each of `names` (`name` or `Container::name`) stands for: its name exactly,
/// in the case written where any is.
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
/// among them; `shown` the files that get cards, whose callers and callees join the seeds there.
pub(super) fn graph(s: &Shared, seeds: &[Seed], shown: &[String]) -> Result<Graph, String> {
    let mut g = Graph::default();
    if seeds.is_empty() {
        return Ok(g);
    }
    let mut code = Reader::new(s);
    let mut lines = vec![
        "**Blast radius** (by name: a call counts for every declaration of its name)".to_string(),
    ];
    let mut mentioned: HashMap<String, Vec<Mention>> = HashMap::new();
    for seed in seeds {
        let sym = &seed.sym;
        let callers = callers(s, &mut mentioned, &sym.name)?.clone();
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
        line.push_str(&format!("; {}", tested(s, &mut mentioned, sym, &callers)?));
        if seed.defs > 1 {
            line.push_str(&format!("; {} declarations share this name", seed.defs));
        }
        lines.push(line);
        let callees = code.callees(sym)?;
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
    let paths = call_paths(&mut code, seeds)?;
    if !paths.is_empty() {
        lines.push(String::new());
        lines.push("**Call paths**".to_string());
        lines.extend(paths.into_iter().map(|p| format!("- {p}")));
    }
    g.summary = lines.join("\n") + "\n\n";
    Ok(g)
}

/// `Container::name`, as explore names a declaration.
pub(super) fn label(sym: &CodeSymbol) -> String {
    match &sym.container {
        Some(c) => format!("{c}::{}", sym.name),
        None => sym.name.clone(),
    }
}

/// Whether two rows are one declaration.
fn same(a: &CodeSymbol, b: &CodeSymbol) -> bool {
    a.rel_path == b.rel_path && a.byte_start == b.byte_start
}

/// The places in code that write `name` inside a declaration other than its own: its callers,
/// read once per explore.
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

/// Whether a test calls `sym`, or a caller of it does, within [`HOPS`] callers.
fn tested(
    s: &Shared,
    mentioned: &mut HashMap<String, Vec<Mention>>,
    sym: &CodeSymbol,
    callers_of: &[Mention],
) -> Result<String, String> {
    let is_test = |m: &Mention| m.symbol.as_ref().is_some_and(|c| c.test);
    if let Some(t) = callers_of.iter().find(|m| is_test(m)) {
        let name = t.symbol.as_ref().map_or("", |c| c.name.as_str());
        return Ok(format!("tested by `{name}` ({})", t.rel_path));
    }
    let mut asked: HashSet<String> = HashSet::from([sym.name.clone()]);
    let mut frontier: Vec<String> = callers_of
        .iter()
        .filter_map(|m| m.symbol.as_ref().map(|c| c.name.clone()))
        .collect();
    for _ in 1..HOPS {
        let mut next = Vec::new();
        for name in frontier
            .into_iter()
            .filter(|n| asked.insert(n.clone()))
            .take(PER_HOP)
        {
            let found = callers(s, mentioned, &name)?;
            if let Some(t) = found.iter().find(|m| is_test(m)) {
                return Ok(format!("tested via `{name}` ({})", t.rel_path));
            }
            next.extend(
                found
                    .iter()
                    .filter_map(|m| m.symbol.as_ref().map(|c| c.name.clone())),
            );
        }
        frontier = next;
    }
    Ok(format!("no tests found within {HOPS} caller hops"))
}

/// `a → b → c` for each seed another seed is reached from by calls, within [`PATH_HOPS`].
fn call_paths(code: &mut Reader, seeds: &[Seed]) -> Result<Vec<String>, String> {
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
            for next in code.callees(&at)? {
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

/// The code files explore reads, each once, masked.
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

    /// The declarations `sym`'s body calls by name, each name's nearest few.
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
