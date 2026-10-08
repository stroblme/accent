//! `explore`: a question answered in one call with the files it is about, best first, each as a
//! card — its outline, the links going out of it and coming in, and the sections holding the
//! question's words verbatim and line-numbered — within a budget of characters, the files past
//! it named below the cards.
//!
//! Ranked from the index alone. A path, a `[[link]]` or a `#tag` in the question names its files
//! outright. The other words find files by their rank over all of them, and each of those scores
//! again by its best passage, a few lines holding the most of the words; a heading holding a word
//! adds to its note; and a file linked to or from two of the best is pulled up beside them, as
//! codegraph's glue is.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::RangeInclusive;

use accent_api::{CodeSymbol, Read};
use accent_core::code;
use accent_core::markdown::{self, LinkKind};
use accent_core::path::{self, FileType};
use rmcp::schemars;
use serde::Deserialize;

use super::graph::{self, Seed};
use super::{Shared, fail, locked};

/// Characters an answer takes when the call names no budget, and the bounds of one it names.
const BUDGET: usize = 16_000;
const BUDGET_MIN: usize = 4_000;
const BUDGET_MAX: usize = 24_000;

/// Words a question is searched by; a longer question is mostly its first words.
const MAX_WORDS: usize = 8;
/// Files the words rank highest that are read for their passages.
const CANDIDATES: usize = 40;
/// Lines a passage spans, and the best passages a card shows.
const PASSAGE: usize = 3;
const PASSAGES: usize = 6;
/// Files a card or a line names, and those a card is likely for.
const SHOWN: usize = 20;
const CARDED: usize = 6;
/// The best files whose links are followed for neighbours.
const SEEDS: usize = 8;
/// Declarations a question's words name that are followed to their callers and callees.
const WORD_SEEDS: usize = 3;
/// A section this many lines long or shorter is shown whole; a longer one as its heading and
/// [`AROUND`] lines either side of each line holding a word.
const WHOLE: usize = 40;
const AROUND: usize = 2;
/// Lines of a file shown when nothing in it holds a word, and of a long declaration's head.
const HEAD: usize = 8;
/// A declaration this many lines long or shorter is shown whole, with up to [`DOC`] lines of
/// comments and attributes above it.
const SYMBOL: usize = 60;
const DOC: usize = 12;
/// Outline lines, links out, backlinks and tags a card lists before it counts the rest, and a
/// card of its own asked for by path.
const LIST: usize = 8;
const LIST_WHOLE: usize = 40;
/// Characters of one line shown, and of a line the words were found on: a note can hold a data
/// URI on one line, and a paragraph is one line too.
const LINE: usize = 300;
const FOUND_LINE: usize = 2_000;

/// Words that say nothing a file could be found by.
const STOP: &[&str] = &[
    "a", "about", "after", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for",
    "from", "has", "have", "how", "i", "if", "in", "into", "is", "it", "its", "me", "my", "not",
    "of", "on", "or", "our", "should", "so", "that", "the", "their", "then", "there", "these",
    "this", "those", "to", "was", "we", "were", "what", "when", "where", "which", "who", "why",
    "with", "would", "you",
];

#[derive(Deserialize, schemars::JsonSchema)]
pub(super) struct ExploreArgs {
    /// What to explore: a question in plain words, names, vault paths, `[[links]]` or `#tags`,
    /// in any mix.
    query: String,
    /// Most characters to answer with: 16000 when left out, 4000 to 24000.
    max_chars: Option<usize>,
}

/// The question taken apart.
#[derive(Default)]
struct Query {
    /// Files a path or a link names, in the question's order.
    pins: Vec<String>,
    tags: Vec<String>,
    words: Vec<String>,
    /// The words written as code is, `Index::backlinks`, `links_from`, `Vault`: declarations'
    /// names, as written.
    names: Vec<String>,
}

impl Query {
    fn pin(&mut self, rel: String) {
        if !self.pins.contains(&rel) {
            self.pins.push(rel);
        }
    }
}

/// The files a question finds, best first, each with what it found there.
type Ranked = Vec<(String, Found)>;

/// What the question found in one file.
#[derive(Default)]
struct Found {
    score: f64,
    /// Lines holding a word.
    hits: usize,
    /// The 1-based lines of its best passages.
    lines: BTreeSet<usize>,
    /// Byte offsets of the headings holding one.
    headings: Vec<usize>,
    /// How many of the best files it links to or is linked from, two or more.
    near: usize,
    title: Option<String>,
    /// The declarations in it to show: named in the question, or calling or called by one.
    symbols: Vec<CodeSymbol>,
}

pub(super) fn explore(s: &Shared, a: &ExploreArgs) -> Result<String, String> {
    let budget = a.max_chars.unwrap_or(BUDGET).clamp(BUDGET_MIN, BUDGET_MAX);
    let partial = s.ready.wait();
    let q = parse(s, &a.query)?;
    let mut out = format!("**Explore: {}**\n\n", a.query.trim());
    if let Some(at) = partial {
        out.push_str(&format!(
            "The index is still being built ({at} files), so this may be incomplete.\n\n"
        ));
    }

    // One file named and nothing else asked: its whole card.
    if let ([rel], true, true) = (q.pins.as_slice(), q.tags.is_empty(), q.words.is_empty()) {
        let found = Found::default();
        out.push_str(&card(
            s,
            rel,
            &found,
            budget.saturating_sub(out.len()),
            true,
        )?);
        return Ok(out);
    }

    let (mut found, seeds) = rank(s, &q)?;
    // The callers and callees of what the question names, shown in the files likeliest to get
    // a card.
    let carded: Vec<String> = found.iter().take(CARDED).map(|(r, _)| r.clone()).collect();
    let graph = graph::graph(s, &seeds, &carded)?;
    for sym in graph.glue {
        if let Some((_, f)) = found.iter_mut().find(|(r, _)| *r == sym.rel_path) {
            f.symbols.push(sym);
        }
    }
    if found.is_empty() {
        out.push_str(
            "Nothing in the vault answers to this. Try other words, a path, a [[link]] or a \
             #tag, or search_notes for an exact phrase.\n",
        );
        return Ok(out);
    }
    out.push_str(&format!("{}, best first.\n\n", count(found.len(), "file")));
    out.push_str(&graph.summary);
    // Cards in rank order while they fit beside a line for each of the files after them, which
    // the first card that does not fit leaves to that list.
    let cap = (budget / 4).max(2_000);
    let mut rest = Vec::new();
    for (i, (rel, f)) in found.iter().enumerate() {
        let room = budget.saturating_sub(out.len() + 80 * (found.len() - i - 1).min(10));
        if rest.is_empty() && room >= 1_000 {
            let card = card(s, rel, f, cap.min(room), false)?;
            if card.len() <= room {
                out.push_str(&card);
                continue;
            }
        }
        rest.push((rel, f));
    }
    if !rest.is_empty() {
        out.push_str("**Also relevant** (explore a path for its card)\n\n");
        for (rel, f) in rest {
            // Any other file's title is its name.
            let title = match (&f.title, path::file_type(rel)) {
                (Some(t), FileType::Note) => format!(" — {t}"),
                _ => String::new(),
            };
            let why = match (f.hits, f.near) {
                (0, 0) => String::new(),
                (0, near) => format!(" (linked with {near} of these)"),
                (hits, _) => format!(" ({})", count(hits, "hit")),
            };
            let line = format!("- `{rel}`{title}{why}\n");
            if out.len() + line.len() > budget {
                break;
            }
            out.push_str(&line);
        }
    }
    Ok(out)
}

/// Take the question apart: `[[links]]` and paths name files, `#tags` name the files holding
/// them, and the rest are words, the ones that say nothing dropped. A path holding spaces is
/// found as the whole question or between backticks.
fn parse(s: &Shared, query: &str) -> Result<Query, String> {
    let mut q = Query::default();
    let whole = query.trim().trim_matches(['`', '"', '\'']);
    if is_file(s, whole) {
        q.pins.push(whole.to_string());
        return Ok(q);
    }
    let mut rest = String::new();
    let mut text = query;
    while let Some(open) = text.find("[[") {
        rest.push_str(&text[..open]);
        let after = &text[open + 2..];
        let Some(close) = after.find("]]") else {
            text = after;
            break;
        };
        let link = after[..close].split('|').next().unwrap_or_default();
        let target = link.split('#').next().unwrap_or_default().trim();
        if let Some(rel) = s.vault.follow(target).map_err(fail)?
            && s.shown(&rel)
        {
            q.pin(rel);
        }
        text = &after[close + 2..];
    }
    rest.push_str(text);
    let mut words = String::new();
    for (i, part) in rest.split('`').enumerate() {
        match i % 2 == 1 && is_file(s, part.trim()) {
            true => q.pin(part.trim().to_string()),
            false => words.extend([part, " "]),
        }
    }
    let tokens: Vec<&str> = words
        .split_whitespace()
        .map(|t| {
            t.trim_start_matches(['"', '\'', '(', '*', '&'])
                .trim_end_matches(['"', '\'', ',', ';', '!', '?', '*', '.', ':', ')'])
                .trim_end_matches("()")
        })
        .filter(|t| !t.is_empty())
        .collect();
    // A few words with nothing to say between them are names, however they are written.
    let said = |t: &str| !STOP.contains(&t.to_lowercase().as_str());
    let bag = tokens.iter().filter(|t| said(t)).count() <= 3
        && (tokens.iter().all(|t| said(t))
            || tokens.iter().enumerate().any(|(i, t)| codey(t, i == 0)));
    for (i, &token) in tokens.iter().enumerate() {
        if let Some(tag) = token.strip_prefix('#').filter(|t| !t.is_empty()) {
            q.tags.push(tag.to_string());
            continue;
        }
        if token.contains(['/', '.']) && is_file(s, token) {
            q.pin(token.to_string());
            continue;
        }
        let name = token
            .chars()
            .all(|c| c.is_alphanumeric() || "_:.".contains(c))
            && (codey(token, i == 0) || bag && said(token));
        if name && !q.names.iter().any(|n| n == token) {
            q.names.push(token.to_string());
        }
        // `Index::backlinks` is searched for as its two words.
        for part in token.split("::").flat_map(|p| p.split('.')) {
            let word = part.to_lowercase();
            if word.chars().count() > 1
                && !STOP.contains(&word.as_str())
                && !q.words.contains(&word)
            {
                q.words.push(word);
            }
        }
    }
    q.words.truncate(MAX_WORDS);
    Ok(q)
}

/// Whether a word is written as code is: `links_from`, `Index::backlinks`, `self.vault`,
/// `camelCase`, a capitalised word not at the question's start.
fn codey(token: &str, first: bool) -> bool {
    let inner = |c: char| token.trim_end_matches(c).contains(c);
    token.contains('_')
        || token.contains("::")
        || inner('.')
        || token.chars().skip(1).any(char::is_uppercase)
        || !first && token.starts_with(char::is_uppercase)
}

/// Whether `rel` names a file in the vault, which a path in the question pins.
fn is_file(s: &Shared, rel: &str) -> bool {
    s.inside(rel).is_ok() && s.vault.resolve(rel).is_ok_and(|p| p.is_file())
}

/// The files the question finds, best first, at most [`SHOWN`], and the declarations it names.
fn rank(s: &Shared, q: &Query) -> Result<(Ranked, Vec<Seed>), String> {
    let mut found: HashMap<String, Found> = HashMap::new();
    let mut cache: HashMap<String, bool> = HashMap::new();
    let mut shown = |rel: &str| *cache.entry(rel.to_string()).or_insert_with(|| s.shown(rel));

    for (i, rel) in q.pins.iter().enumerate() {
        found.entry(rel.clone()).or_default().score += 100.0 - i as f64;
    }
    // A declaration named comes right after a file named.
    let mut seeds = graph::seeds(s, &q.names)?;
    for (i, seed) in seeds.iter().enumerate() {
        let e = found.entry(seed.sym.rel_path.clone()).or_default();
        e.score += 50.0 - i as f64;
        e.symbols.push(seed.sym.clone());
    }
    // So do the declarations named by the question's words, `settle_index` for "settle the
    // index"; one named by two words or more is followed as one the question names.
    if !q.words.is_empty() {
        let named = s.vault.symbols_by_words(&q.words, 12).map_err(fail)?;
        for (sym, held) in named.into_iter().filter(|(c, _)| shown(&c.rel_path)) {
            let e = found.entry(sym.rel_path.clone()).or_default();
            e.score += 2.0 * held as f64 / q.words.len() as f64;
            if held >= 2 && q.names.is_empty() && seeds.len() < WORD_SEEDS {
                seeds.push(Seed {
                    sym: sym.clone(),
                    defs: 1,
                });
            }
            e.symbols.push(sym);
        }
    }
    for tag in &q.tags {
        for f in s.vault.files_with_tag(tag).map_err(fail)? {
            if shown(&f.rel_path) {
                let e = found.entry(f.rel_path).or_default();
                e.score += 1.0;
                e.title = f.title;
            }
        }
    }

    if !q.words.is_empty() {
        // The files the words rank highest, each scored again by its best passage.
        let ranked = {
            let _turn = locked(&s.search);
            s.vault.rank_files(&q.words, CANDIDATES).map_err(fail)?
        };
        let mut candidates: Vec<String> = Vec::new();
        for (rel, title) in ranked {
            if shown(&rel) {
                let e = found.entry(rel.clone()).or_default();
                e.score += 1.0 / (1.0 + candidates.len() as f64 / 4.0);
                e.title = title;
                candidates.push(rel);
            }
        }
        passages(s, &q.words, &candidates, &mut found);
        for h in s.vault.headings_matching(&q.words, 100).map_err(fail)? {
            if shown(&h.rel_path) {
                let e = found.entry(h.rel_path).or_default();
                e.score += 0.5 * h.words as f64 / q.words.len() as f64;
                e.headings.push(h.byte_start as usize);
            }
        }
    }

    // The neighbours of the best files: one linked to or from two of them is part of the
    // answer, found or not.
    let mut best: Vec<(&String, f64)> = found.iter().map(|(rel, f)| (rel, f.score)).collect();
    best.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let mut near: HashMap<String, HashSet<String>> = HashMap::new();
    for (rel, _) in best.into_iter().take(SEEDS) {
        let out = s.vault.links_from(rel).map_err(fail)?;
        let to = out.into_iter().filter_map(|l| l.path);
        let from = s.vault.backlinks(rel).map_err(fail)?;
        for other in to.chain(from.into_iter().map(|b| b.src_rel_path)) {
            if &other != rel {
                near.entry(other).or_default().insert(rel.clone());
            }
        }
    }
    // A file no word, tag or path found stays below every one that was.
    for (rel, seeds) in near {
        if seeds.len() >= 2 && shown(&rel) {
            let e = found.entry(rel).or_default();
            let weight = if e.hits > 0 || e.score >= 1.0 {
                0.3
            } else {
                0.05
            };
            e.score += weight * seeds.len() as f64;
            e.near = seeds.len();
        }
    }

    let mut found: Ranked = found.into_iter().collect();
    found.sort_by(|a, b| {
        (b.1.score.total_cmp(&a.1.score))
            .then(b.1.hits.cmp(&a.1.hits))
            .then_with(|| a.0.cmp(&b.0))
    });
    found.truncate(SHOWN);
    Ok((found, seeds))
}

/// Score each candidate by its best passage, [`PASSAGE`] lines holding the most of `words`, a
/// word fewer of the candidates hold counting for more, and keep the lines of its best few:
/// a question is answered by a paragraph, which a long file's rank over the words buries.
fn passages(
    s: &Shared,
    words: &[String],
    candidates: &[String],
    found: &mut HashMap<String, Found>,
) {
    let mut masks: Vec<(&String, Vec<u32>)> = Vec::new();
    let mut held = vec![0usize; words.len()];
    for rel in candidates {
        let Ok(Read::Text(t)) = s.vault.read_text(rel) else {
            continue;
        };
        let lines: Vec<u32> = t
            .text
            .lines()
            .map(|l| mask(&l.to_lowercase(), words))
            .collect();
        let all = lines.iter().fold(0, |a, m| a | m);
        for (i, n) in held.iter_mut().enumerate() {
            *n += usize::from(all & (1 << i) != 0);
        }
        masks.push((rel, lines));
    }
    let files = masks.len() as f64;
    let weight: Vec<f64> = held
        .iter()
        .map(|&n| (1.0 + files / (1.0 + n as f64)).ln())
        .collect();
    let total: f64 = weight.iter().sum();
    let score = |m: u32| -> f64 {
        (0..words.len())
            .filter(|i| m & (1 << i) != 0)
            .map(|i| weight[i])
            .sum()
    };
    for (rel, lines) in masks {
        let windows: Vec<f64> = (0..lines.len())
            .map(|i| {
                score(
                    lines[i..(i + PASSAGE).min(lines.len())]
                        .iter()
                        .fold(0, |a, m| a | m),
                )
            })
            .collect();
        let best = windows.iter().copied().fold(0.0, f64::max);
        let Some(e) = found.get_mut(rel).filter(|_| best > 0.0) else {
            continue;
        };
        e.score += 2.0 * best / total;
        e.hits = lines.iter().filter(|&&m| m != 0).count();
        let mut top: Vec<usize> = (0..lines.len())
            .filter(|&i| lines[i] != 0 && windows[i] >= 0.75 * best)
            .collect();
        top.sort_by(|a, b| windows[*b].total_cmp(&windows[*a]));
        for i in top.into_iter().take(PASSAGES) {
            let end = (i + PASSAGE).min(lines.len());
            e.lines
                .extend((i..end).filter(|&j| lines[j] != 0).map(|j| j + 1));
        }
    }
}

/// Which of `words` the lower-cased `line` holds, each as the start of a word, as bits.
fn mask(line: &str, words: &[String]) -> u32 {
    let mut m = 0;
    for (i, w) in words.iter().enumerate() {
        let starts = line.match_indices(w.as_str()).any(|(at, _)| {
            !line[..at]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
        });
        m |= u32::from(starts) << i;
    }
    m
}

/// One file's card, at most about `cap` characters: its outline, its links out and in, and its
/// text — the lines `f` found and the sections around them, or all of it when `whole`, as far
/// as `cap` goes.
fn card(s: &Shared, rel: &str, f: &Found, cap: usize, whole: bool) -> Result<String, String> {
    let mut out = format!("### `{rel}`");
    let max = if whole { LIST_WHOLE } else { LIST };
    let text = match s.vault.read_text(rel).map_err(fail)? {
        Read::Text(t) => t.text,
        Read::Binary { size } => {
            out.push_str(&format!(" — binary, {size} bytes\n"));
            backlinks(s, rel, &mut out, max)?;
            return Ok(out + "\n");
        }
        Read::TooLarge { size } => return Ok(out + &format!(" — {size} bytes, too large\n\n")),
    };
    let note = path::file_type(rel) == FileType::Note;
    let analysis = note.then(|| markdown::analyze(&text));
    let starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .filter(|&i| i < text.len() || i == 0)
        .collect();
    let line_of = |byte: usize| starts.partition_point(|&s| s <= byte);

    if let Some(title) = analysis.as_ref().and_then(|a| a.title.as_ref()) {
        out.push_str(&format!(" — {title}"));
    }
    out.push_str(&format!(" ({} lines)\n", starts.len()));
    let mut heads = Vec::new();
    if let Some(a) = &analysis {
        let tags: BTreeSet<&str> = a.tags.iter().map(|t| t.name.as_str()).collect();
        let mut shown: Vec<String> = tags.iter().take(max).map(|t| format!("#{t}")).collect();
        if !shown.is_empty() {
            if tags.len() > shown.len() {
                shown.push(format!("+{} more", tags.len() - shown.len()));
            }
            out.push_str(&format!("Tags: {}\n", shown.join(" ")));
        }
        heads = a.headings.iter().map(|h| line_of(h.range.start)).collect();
        let outline = a.headings.iter().map(|h| {
            let marks = "#".repeat(h.level as usize);
            format!("{}  {marks} {}", line_of(h.range.start), h.text)
        });
        list(&mut out, "Outline", outline.collect(), max);
    }
    let symbols = match code::lang_of(rel) {
        Some(_) => s.vault.file_symbols(rel).map_err(fail)?,
        None => Vec::new(),
    };
    let outline = symbols
        .iter()
        .map(|c| format!("{}  {} {}", c.line, c.kind.label(), graph::label(c)));
    list(&mut out, "Outline", outline.collect(), max);
    let links = s.vault.links_from(rel).map_err(fail)?;
    let links = links
        .into_iter()
        .filter(|l| l.kind != LinkKind::External)
        .map(|l| {
            let to = match &l.path {
                Some(p) => format!("`{p}`"),
                None => format!("{} (missing)", l.target),
            };
            let anchor = l.anchor.map(|a| format!(" #{a}")).unwrap_or_default();
            format!("{} → {to}{anchor}", line_of(l.byte_start as usize))
        });
    list(&mut out, "Links out", links.collect(), max);
    backlinks(s, rel, &mut out, max)?;

    let mut lines = f.lines.clone();
    lines.extend(f.headings.iter().map(|&b| line_of(b)));
    let all: Vec<&str> = text.lines().collect();
    let ranges = match (whole, symbols.is_empty()) {
        (true, _) => vec![1..=all.len().max(1)],
        (false, true) => shown_lines(all.len(), &heads, &lines),
        (false, false) => code_lines(&all, &symbols, &f.symbols, &lines),
    };
    let fence = "`".repeat(longest_tick_run(&text).max(2) + 1);
    let lang = match note {
        true => "markdown",
        false => std::path::Path::new(rel)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default(),
    };
    out.push_str(&format!("{fence}{lang}\n"));
    // Line by line while the line, the word on where the rest is, and the fence fit the cap.
    let end = format!("{fence}\n\n");
    let cut = format!("... (cut: read_note `{rel}` for the rest)\n");
    let mut last = 0;
    'ranges: for range in ranges {
        for n in range {
            let Some(line) = all.get(n - 1) else { break };
            let gap = if last > 0 && n > last + 1 {
                "... (gap) ...\n"
            } else {
                ""
            };
            let room = if whole || lines.contains(&n) {
                FOUND_LINE
            } else {
                LINE
            };
            let shown = &line[..line.floor_char_boundary(room)];
            let more = if shown.len() < line.len() { "…" } else { "" };
            let row = format!("{gap}{n}\t{shown}{more}\n");
            if out.len() + row.len() + cut.len() + end.len() > cap {
                out.push_str(&cut);
                break 'ranges;
            }
            out.push_str(&row);
            last = n;
        }
    }
    out.push_str(&format!("{fence}\n\n"));
    Ok(out)
}

/// The notes linking to `rel`, each with its line, at most `max` of them read for it.
fn backlinks(s: &Shared, rel: &str, out: &mut String, max: usize) -> Result<(), String> {
    let rows = s.vault.backlinks(rel).map_err(fail)?;
    let total = rows.len();
    let at = rows
        .into_iter()
        .take(max)
        .map(|b| (b.src_rel_path, b.byte_start as usize));
    let quoted = s.lines(at).into_iter().map(|(src, line, text)| {
        let text: String = text.chars().take(100).collect();
        format!("`{src}`:{line}  {text}")
    });
    let mut quoted: Vec<String> = quoted.collect();
    let more = total - quoted.len().min(total);
    quoted.extend((more > 0).then(|| format!("+{more} more")));
    list(out, "Backlinks", quoted, usize::MAX);
    Ok(())
}

/// A titled list of a card's: at most `max` of the rows, and how many more there are.
fn list(out: &mut String, title: &str, rows: Vec<String>, max: usize) {
    if rows.is_empty() {
        return;
    }
    out.push_str(&format!("{title}:\n"));
    for row in rows.iter().take(max) {
        out.push_str(&format!("  {row}\n"));
    }
    if rows.len() > max {
        out.push_str(&format!("  +{} more\n", rows.len() - max));
    }
}

/// The 1-based line ranges of a file of `len` lines to show for the `found` lines, `heads`
/// being its headings' lines: the section each sits in when it is short, else the heading and
/// a few lines either side; the file's first lines when nothing was found in it.
fn shown_lines(len: usize, heads: &[usize], found: &BTreeSet<usize>) -> Vec<RangeInclusive<usize>> {
    if len == 0 {
        return Vec::new();
    }
    let mut ranges: Vec<RangeInclusive<usize>> = Vec::new();
    for &n in found.iter().filter(|&&n| n >= 1 && n <= len) {
        let start = heads.iter().rev().find(|&&h| h <= n).copied().unwrap_or(1);
        let end = heads.iter().find(|&&h| h > n).map_or(len, |&h| h - 1);
        match end - start < WHOLE {
            true => ranges.push(start..=end),
            false => {
                ranges.push(start..=start);
                ranges.push(n.saturating_sub(AROUND).max(start)..=(n + AROUND).min(end));
            }
        }
    }
    if ranges.is_empty() {
        ranges.push(1..=len.min(HEAD));
    }
    merge(ranges)
}

/// The 1-based line ranges of a code file (`all` its lines) to show: each declaration in `show`
/// and the innermost one around each `found` line, from the comments and attributes above it to
/// its end — a long one as its head, the found lines and its last line — and a found line no
/// declaration holds with the lines around it.
fn code_lines(
    all: &[&str],
    symbols: &[CodeSymbol],
    show: &[CodeSymbol],
    found: &BTreeSet<usize>,
) -> Vec<RangeInclusive<usize>> {
    let len = all.len();
    let mut ranges: Vec<RangeInclusive<usize>> = Vec::new();
    let mut whole: Vec<&CodeSymbol> = show.iter().collect();
    for &n in found.iter().filter(|&&n| n >= 1 && n <= len) {
        let inner = symbols
            .iter()
            .filter(|c| (c.line as usize) <= n && n <= c.end_line as usize)
            .min_by_key(|c| c.end_line - c.line);
        match inner {
            Some(c) => whole.push(c),
            None => ranges.push(n.saturating_sub(AROUND).max(1)..=(n + AROUND).min(len)),
        }
    }
    for c in whole {
        let (line, end) = (c.line as usize, (c.end_line as usize).min(len));
        // Its doc comments and attributes.
        let mut start = line;
        while start > 1 && start + DOC > line && is_preamble(all[start - 2]) {
            start -= 1;
        }
        if end - start < SYMBOL {
            ranges.push(start..=end);
            continue;
        }
        ranges.push(start..=line + HEAD);
        for &n in found.range(line..=end) {
            ranges.push(n.saturating_sub(AROUND).max(line)..=(n + AROUND).min(end));
        }
        ranges.push(end..=end);
    }
    if ranges.is_empty() {
        ranges.push(1..=len.min(HEAD));
    }
    merge(ranges)
}

/// Whether a line above a declaration belongs to it: a comment, an attribute, an annotation.
fn is_preamble(line: &str) -> bool {
    let line = line.trim_start();
    ["///", "//", "/*", "*", "#[", "@", "#"]
        .iter()
        .any(|p| line.starts_with(p))
}

/// Sorted, overlapping and touching ranges made one.
fn merge(mut ranges: Vec<RangeInclusive<usize>>) -> Vec<RangeInclusive<usize>> {
    ranges.sort_by_key(|r| *r.start());
    let mut merged: Vec<RangeInclusive<usize>> = Vec::new();
    for r in ranges {
        match merged.last_mut() {
            Some(m) if *r.start() <= m.end() + 1 => *m = *m.start()..=(*m.end()).max(*r.end()),
            _ => merged.push(r),
        }
    }
    merged
}

/// `n` of `what`, in the plural but for one.
pub(super) fn count(n: usize, what: &str) -> String {
    match n {
        1 => format!("1 {what}"),
        n => format!("{n} {what}s"),
    }
}

/// The longest run of backticks in `text`: the fence around it has to be longer.
fn longest_tick_run(text: &str) -> usize {
    text.split(|c| c != '`').map(str::len).max().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A short section is shown whole, a long one around the line, and neighbours merge.
    #[test]
    fn sections_are_shown_whole_or_around_the_line() {
        let found: BTreeSet<usize> = [5, 6].into();
        assert_eq!(shown_lines(20, &[1, 4, 10], &found), [4..=9]);
        let found: BTreeSet<usize> = [60].into();
        assert_eq!(shown_lines(100, &[1, 2], &found), [2..=2, 58..=62]);
        assert_eq!(shown_lines(30, &[], &BTreeSet::new()), [1..=HEAD]);
    }
}
