//! What a code file brings in from other files, read as the scanner reads declarations: `mod`
//! and `use`, `import` and `from`, `require`, `#include`. Each is a path the index later puts a
//! file of the vault to ([`rank`], `Index`'s `imports` table), which is what the graph's import
//! edges are; a name nothing in the vault answers to stays unresolved and is never drawn.
//!
//! A path is either exact, a vault path without the file's extension (Rust's modules, a script's
//! `./x`), or near: one that any folder may end in, the nearest to the importer winning
//! (Python's `a.b`, a C header, a Go package), which is how a vault of many projects puts each
//! project's `utils.py` to its own importers.

use super::{Lang, is_test_path, lang_of, typst};
use crate::path;
use regex::Regex;
use std::ops::Range;
use std::sync::LazyLock;

/// One path a file imports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// The vault path without an extension, or with the one it was written with where the
    /// language writes one (`#include "a.h"`, `./x.js`); one any folder may end in when `near`;
    /// one in the folder of the crate `krate` names where it is set.
    pub target: String,
    pub near: bool,
    /// [`module_key`] of `target`: what a file has to be called to be one of its candidates.
    pub key: String,
    /// [`module_key`] of the folder `target` is in, where its last name may be an item of the
    /// module above rather than a module (`use super::reconcile::link_kind_of`, `from m import f`).
    pub up_key: Option<String>,
    /// [`module_key`] of the folder above that one, where the last two names may be a module
    /// written inline and its item (`use crate::index::testing::fixture`). Rust's alone.
    pub up2_key: Option<String>,
    /// The crate a Rust `use` of another crate names (`accent_core`), which the index finds by
    /// the name its `Cargo.toml` gives it ([`crate_name`]): `target` is then in the crate's
    /// folder, `src/index` for `use accent_core::index`.
    pub krate: Option<String>,
}

/// The paths `text`, read as `lang` from the file `rel`, imports, each once, in the order it
/// writes them; `masked` as [`super::mask`] leaves it, which is where keywords are looked for,
/// while what a string says is read from `text` at the same offsets.
pub fn imports(lang: Lang, rel: &str, masked: &str, text: &str) -> Vec<Import> {
    let mut out = Vec::new();
    match lang {
        Lang::Rust => rust(rel, masked, text, &mut out),
        Lang::Python => python(rel, masked, &mut out),
        Lang::Script => script(rel, masked, text, &mut out),
        Lang::C => c(masked, text, &mut out),
        Lang::Go => go(masked, text, &mut out),
        Lang::Java | Lang::Kotlin => java(masked, &mut out),
        Lang::Lean => lean(masked, &mut out),
        Lang::Typst => {
            for spec in typst::includes(text) {
                match spec.strip_prefix('/') {
                    _ if spec.starts_with('@') => {}
                    Some(rooted) => push(&mut out, rooted.to_string(), true, 0),
                    None => push(
                        &mut out,
                        path::resolve(path::parent_dir(rel), &spec),
                        false,
                        0,
                    ),
                }
            }
        }
    }
    out
}

/// What a file is looked up by when it is a candidate for an import, and what an import's target
/// is: its name up to the first `.`, but a Go file's, an `__init__.py`'s, a `mod.rs`'s, a crate
/// root's (`lib.rs`, `main.rs`) and a script's `index`, which stand for their folder, and so take
/// its name. Lower-cased, which only narrows the lookup: [`rank`] compares paths as written.
/// `None` where that leaves nothing.
pub fn module_key(path: &str) -> Option<String> {
    let name = path::basename(path);
    let folder = name.ends_with(".go")
        || matches!(
            name,
            "mod.rs" | "lib.rs" | "main.rs" | "__init__.py" | "__init__.pyi"
        )
        || name
            .split_once('.')
            .is_some_and(|(stem, ext)| stem == "index" && SCRIPT_EXT.contains(&ext));
    let key = match folder {
        true => path::basename(path::parent_dir(path)),
        false => name.split('.').next().unwrap_or_default(),
    };
    (!key.is_empty()).then(|| key.to_lowercase())
}

const SCRIPT_EXT: [&str; 8] = ["js", "jsx", "ts", "tsx", "mjs", "cjs", "mts", "cts"];

/// How well the file `cand` answers the import of `target` that `src` writes, smaller better,
/// `None` for not at all. The import's own path before the modules above it (`ups` of them),
/// then the file names its language gives a module in their order, then the folders between the
/// two files; a Go package, a folder, by how much of its path the file's folder ends in. A file
/// never answers its own import. What the index resolves its imports by, as the SQL function
/// `import_rank`.
pub fn rank(src: &str, target: &str, near: bool, ups: usize, cand: &str) -> Option<i64> {
    let lang = lang_of(src)?;
    if cand == src {
        return None;
    }
    if lang == Lang::Go {
        return go_rank(src, target, cand);
    }
    let above = std::iter::successors(Some(target), |t| Some(path::parent_dir(t)))
        .skip(1)
        .take(ups)
        .take_while(|p| !p.is_empty());
    for (reading, base) in std::iter::once(target).chain(above).enumerate() {
        for (form, want) in forms(lang, base).iter().enumerate() {
            let found = match near {
                true => cand == want || cand.ends_with(&format!("/{want}")),
                false => cand == want,
            };
            if found {
                let distance = if near { distance(src, cand) } else { 0 };
                return Some((reading as i64 * 100 + form as i64) * 1000 + distance);
            }
        }
    }
    None
}

/// The paths a module at `base` may be, best first, as `lang` names its files.
fn forms(lang: Lang, base: &str) -> Vec<String> {
    let ends: &[&str] = match lang {
        // A crate's own folder is its root's, `src/lib.rs` or `src/main.rs`.
        Lang::Rust => &[".rs", "/mod.rs", "/lib.rs", "/main.rs"],
        Lang::Python => &[".py", ".pyi", "/__init__.py"],
        Lang::Script => &[
            "",
            ".ts",
            ".tsx",
            ".d.ts",
            ".js",
            ".jsx",
            ".mjs",
            ".cjs",
            ".mts",
            ".cts",
            "/index.ts",
            "/index.tsx",
            "/index.js",
            "/index.jsx",
            "/index.mjs",
        ],
        Lang::Java | Lang::Kotlin => &[".java", ".kt"],
        Lang::Lean => &[".lean"],
        Lang::C | Lang::Typst | Lang::Go => &[""],
    };
    let mut forms: Vec<String> = ends.iter().map(|end| format!("{base}{end}")).collect();
    if lang == Lang::Script {
        for (js, ts) in COMPILED {
            if let Some(stem) = base.strip_suffix(js) {
                forms.extend(ts.iter().map(|ts| format!("{stem}{ts}")));
            }
        }
    }
    forms
}

/// TypeScript imports a module by the name it compiles to: `./x.js` is the `x.ts` beside it.
const COMPILED: [(&str, &[&str]); 4] = [
    (".js", &[".ts", ".tsx"]),
    (".jsx", &[".tsx"]),
    (".mjs", &[".mts"]),
    (".cjs", &[".cts"]),
];

/// A Go import names a package, which is a folder: `cand` is one of its files when its folder's
/// path ends in at least the import path's last two names, `"fmt"` and `"os"` never resolving,
/// and the more of them it ends in the better.
fn go_rank(src: &str, target: &str, cand: &str) -> Option<i64> {
    if !cand.ends_with(".go") || is_test_path(cand) {
        return None;
    }
    let want: Vec<&str> = target.split('/').collect();
    let have = path::parent_dir(cand).split('/');
    let matched = want
        .iter()
        .rev()
        .zip(have.rev())
        .take_while(|(a, b)| *a == b)
        .count();
    (matched >= 2).then(|| (want.len() - matched) as i64 * 1000 + distance(src, cand))
}

/// The folders up from `src`'s to the one both files are under, and down to `cand`'s, at most 999.
fn distance(src: &str, cand: &str) -> i64 {
    fn folders(rel: &str) -> impl Iterator<Item = &str> {
        path::parent_dir(rel).split('/').filter(|s| !s.is_empty())
    }
    let (a, b) = (folders(src).count(), folders(cand).count());
    let shared = folders(src)
        .zip(folders(cand))
        .take_while(|(x, y)| x == y)
        .count();
    (a + b - 2 * shared).min(999) as i64
}

/// Add the import of `target`, whose last `ups` names (two at most) may be items rather than
/// modules, unless `out` has it.
fn push(out: &mut Vec<Import>, target: String, near: bool, ups: usize) {
    push_in(out, None, target, near, ups);
}

/// [`push`] for a path in the folder of the crate `krate`, where it is set.
fn push_in(out: &mut Vec<Import>, krate: Option<String>, target: String, near: bool, ups: usize) {
    let Some(key) = module_key(&target) else {
        return;
    };
    let up = path::parent_dir(&target);
    let import = Import {
        key,
        up_key: (ups >= 1).then(|| module_key(up)).flatten(),
        up2_key: (ups >= 2)
            .then(|| module_key(path::parent_dir(up)))
            .flatten(),
        target,
        near,
        krate,
    };
    if !out.contains(&import) {
        out.push(import);
    }
}

fn join(dir: &str, rest: &str) -> String {
    match (dir.is_empty(), rest.is_empty()) {
        (true, _) => rest.to_string(),
        (_, true) => dir.to_string(),
        _ => format!("{dir}/{rest}"),
    }
}

fn regex(re: &str) -> Regex {
    Regex::new(re).expect("a valid import pattern")
}

/// The string literal at `text[at..]` after spaces (and a `(` where `paren`), quoted with `'` or
/// `"`, or `<…>` where `angle`: what a keyword the masked text holds is followed by.
fn literal(text: &str, at: usize, paren: bool, angle: bool) -> Option<&str> {
    let mut rest = text[at..].trim_start_matches([' ', '\t']);
    if paren {
        rest = rest.strip_prefix('(').unwrap_or(rest).trim_start();
    }
    let close = match rest.chars().next()? {
        q @ ('"' | '\'') => q,
        '<' if angle => '>',
        _ => return None,
    };
    let body = &rest[1..];
    Some(&body[..body.find([close, '\n'])?])
}

// ---------------------------------------------------------------------------------------- Rust

/// The folder a Rust file's child modules are in: its own for `mod.rs`, `lib.rs` and `main.rs`,
/// else the one named after it beside it.
fn module_dir(rel: &str) -> String {
    match path::basename(rel) {
        "mod.rs" | "lib.rs" | "main.rs" => path::parent_dir(rel).to_string(),
        _ => join(path::parent_dir(rel), &path::stem(rel)),
    }
}

/// The folder `crate::` names: the nearest `src` above the file, or a `lib.rs`'s or `main.rs`'s
/// own outside one (an example's).
fn crate_dir(rel: &str) -> Option<String> {
    let mut dir = path::parent_dir(rel);
    while !dir.is_empty() {
        if path::basename(dir) == "src" {
            return Some(dir.to_string());
        }
        dir = path::parent_dir(dir);
    }
    matches!(path::basename(rel), "lib.rs" | "main.rs").then(|| path::parent_dir(rel).to_string())
}

/// `mod x;`, read at the path a `#[path = "…"]` before it gives where there is one, and
/// `use crate::…`, `self::…`, `super::…`, a module the file declares (`use index::Index` beside
/// `mod index;`) and another crate's, the standard library's aside. The trailing names of types,
/// traits and constants are left out, as they start upper-case: `use crate::index::{Index,
/// links}` imports `index` and `index::links`. A `mod x { … }` written in the file is a module the
/// paths inside it start from, and one they never leave is no import.
fn rust(rel: &str, masked: &str, text: &str, out: &mut Vec<Import>) {
    static MOD: LazyLock<Regex> = LazyLock::new(|| {
        regex(
            r"(?m)^[ \t]*((?:#\[[^\n]*\]\s*)*)(?:pub(?:[ \t]*\([^)\n]*\))?[ \t]+)?mod[ \t]+(\w+)[ \t]*;",
        )
    });
    // Not past the `=`: the string after it is blanked to spaces, which `literal` reads past.
    static PATH: LazyLock<Regex> = LazyLock::new(|| regex(r"#\[[ \t]*path[ \t]*="));
    static USE: LazyLock<Regex> = LazyLock::new(|| {
        regex(r"(?m)^[ \t]*(?:pub(?:[ \t]*\([^)\n]*\))?[ \t]+)?use[ \t]+([^;]*);")
    });
    static AS: LazyLock<Regex> = LazyLock::new(|| regex(r"\s+as\s+\w+"));
    let inline = inline_mods(masked);
    let around = |at: usize| -> Vec<&str> {
        let names = inline.iter().filter(|(span, _)| span.contains(&at));
        names.map(|(_, name)| *name).collect()
    };
    // What `use name::…` may start from outside every inline module besides a crate.
    let mut declared: Vec<&str> = Vec::new();
    for c in MOD.captures_iter(masked) {
        let (attrs, name) = (c.get(1).expect("the attributes"), &c[2]);
        let within = around(attrs.start());
        if within.is_empty() {
            declared.push(c.get(2).expect("the name").as_str());
        }
        // Next to the file, wherever the file is (the Rust reference's rule outside an inline
        // module, which this applies inside one too).
        let file = PATH
            .find(attrs.as_str())
            .and_then(|m| literal(text, attrs.start() + m.end(), false, false));
        if let Some(file) = file {
            let target = path::resolve(path::parent_dir(rel), file);
            let target = target.strip_suffix(".rs").unwrap_or(&target);
            let target = target.strip_suffix("/mod").unwrap_or(target);
            push(out, target.to_string(), false, 0);
            continue;
        }
        let mut dir = module_dir(rel);
        for name in within {
            dir = join(&dir, name);
        }
        push(out, join(&dir, name), false, 0);
    }
    let outermost = inline
        .iter()
        .filter(|(span, _)| around(span.start).len() == 1);
    declared.extend(outermost.map(|(_, name)| *name));
    for c in USE.captures_iter(masked) {
        let tree: String = AS.replace_all(&c[1], "").split_whitespace().collect();
        let depth = around(c.get(0).expect("the match").start()).len();
        let mut paths = Vec::new();
        expand(&tree, &[], &mut paths);
        for segments in paths {
            if let Some((krate, target, ups)) = rust_path(rel, &segments, depth, &declared) {
                push_in(out, krate, target, false, ups);
            }
        }
    }
}

/// The modules written inline, `mod name { … }`, by the span of their braces.
fn inline_mods(masked: &str) -> Vec<(Range<usize>, &str)> {
    static INLINE: LazyLock<Regex> = LazyLock::new(|| regex(r"\bmod[ \t]+(\w+)[ \t]*\{"));
    let b = masked.as_bytes();
    let mut mods = Vec::new();
    for c in INLINE.captures_iter(masked) {
        let open = c.get(0).expect("the match").end() - 1;
        let mut depth = 0;
        let close = (open..b.len()).find(|&i| {
            depth += match b[i] {
                b'{' => 1,
                b'}' => -1,
                _ => 0,
            };
            depth == 0
        });
        mods.push((
            open..close.unwrap_or(b.len()),
            c.get(1).expect("the name").as_str(),
        ));
    }
    mods
}

/// The paths a `use` tree with its spaces taken out names, each as its names: `a::{b, c::{d}}`
/// is `a::b` and `a::c::d`. A `self` or `*` at the end is the module before it.
fn expand(tree: &str, prefix: &[String], out: &mut Vec<Vec<String>>) {
    let Some(open) = tree.find('{') else {
        let mut path = prefix.to_vec();
        path.extend(
            tree.split("::")
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        );
        if matches!(path.last().map(String::as_str), Some("self" | "*")) {
            path.pop();
        }
        if !path.is_empty() {
            out.push(path);
        }
        return;
    };
    let mut head = prefix.to_vec();
    head.extend(
        tree[..open]
            .split("::")
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    );
    let inner = &tree[open + 1..tree.rfind('}').unwrap_or(tree.len()).max(open + 1)];
    let (mut depth, mut start) = (0, 0);
    for (i, ch) in inner.char_indices().chain([(inner.len(), ',')]) {
        match ch {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                if start < i {
                    expand(&inner[start..i], &head, out);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
}

/// The target one `use` path names from `rel`, `depth` inline modules in, where the file declares
/// the modules `declared`: the crate it is in where it names another one, the path, and how many
/// of its last names may be items or inline modules rather than module files.
fn rust_path(
    rel: &str,
    segments: &[String],
    depth: usize,
    declared: &[&str],
) -> Option<(Option<String>, String, usize)> {
    let supers = segments.iter().take_while(|s| *s == "super").count();
    let first = segments.first()?.as_str();
    let (krate, base, rest) = match first {
        "crate" => (None, crate_dir(rel)?, &segments[1..]),
        "self" if depth == 0 => (None, module_dir(rel), &segments[1..]),
        // Past the inline modules it is written in, each `super` is a folder up.
        "super" if supers > depth => {
            let mut dir = module_dir(rel);
            for _ in depth..supers {
                dir = path::parent_dir(&dir).to_string();
            }
            (None, dir, &segments[supers..])
        }
        _ if depth == 0 && declared.contains(&first) => (None, module_dir(rel), segments),
        // The crates every program has, which no vault holds, and a type or a variant in scope.
        "std" | "core" | "alloc" | "proc_macro" | "test" | "self" | "super" => return None,
        _ if first.starts_with(char::is_uppercase) => return None,
        _ => (Some(first.to_string()), "src".to_string(), &segments[1..]),
    };
    let modules = rest
        .iter()
        .rposition(|s| !s.starts_with(char::is_uppercase));
    let rest = &rest[..modules.map_or(0, |i| i + 1)];
    // `crate::Item` is the crate's root, which every module of it uses, and so is `super::Item`
    // or `super::*` where `super` is the root (user decision 2026-11-24): drawn, the root would be
    // a hub of the graph. Another crate's root is what the file depends on.
    if rest.is_empty() && krate.is_none() && crate_dir(rel).as_ref() == Some(&base) {
        return None;
    }
    Some((krate, join(&base, &rest.join("/")), rest.len().min(2)))
}

/// The name a `use` gives the library crate the manifest `text` makes, its `[lib] name` or its
/// package's with `-` read as `_`; `None` for a manifest that makes none, a workspace's own.
pub fn crate_name(text: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Manifest {
        package: Option<Named>,
        lib: Option<Named>,
    }
    #[derive(serde::Deserialize)]
    struct Named {
        name: Option<String>,
    }
    let manifest: Manifest = toml::from_str(text).ok()?;
    let lib = manifest.lib.and_then(|lib| lib.name);
    let package = manifest.package.and_then(|package| package.name);
    Some(lib.or(package)?.replace('-', "_"))
}

// -------------------------------------------------------------------------------------- Python

/// `import a.b, c as d` and `from m import x, (y, z)`, a relative `from .m` from the file's own
/// folder up, exact, an absolute one near; `from m import *` imports `m`.
fn python(rel: &str, masked: &str, out: &mut Vec<Import>) {
    static IMPORT: LazyLock<Regex> = LazyLock::new(|| regex(r"(?m)^[ \t]*import[ \t]+([^\n]+)"));
    static FROM: LazyLock<Regex> = LazyLock::new(|| {
        regex(r"(?m)^[ \t]*from[ \t]+(\.*)([\w.]*)[ \t]+import[ \t]*(\([^)]*\)|[^\n]*)")
    });
    let dotted = |name: &str| {
        (!name.is_empty()
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '.'))
        .then(|| name.replace('.', "/"))
    };
    let first = |part: &str| {
        part.split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string()
    };
    let mut found: Vec<(usize, String, bool, bool)> = Vec::new();
    for c in IMPORT.captures_iter(masked) {
        let at = c.get(0).expect("the match").start();
        for name in c[1].split(',').filter_map(|part| dotted(&first(part))) {
            found.push((at, name, true, false));
        }
    }
    for c in FROM.captures_iter(masked) {
        let at = c.get(0).expect("the match").start();
        let (dots, module) = (c[1].len(), c[2].replace('.', "/"));
        let base = match dots {
            0 if module.is_empty() => continue,
            0 => module,
            _ => {
                let mut dir = path::parent_dir(rel);
                for _ in 1..dots {
                    dir = path::parent_dir(dir);
                }
                match module.is_empty() {
                    true => dir.to_string(),
                    false => join(dir, &module),
                }
            }
        };
        let names = c[3].trim_matches(|ch: char| ch == '(' || ch == ')' || ch.is_whitespace());
        for name in names.split(',').map(first).filter(|n| !n.is_empty()) {
            match name.as_str() {
                "*" => found.push((at, base.clone(), dots == 0, false)),
                _ => found.push((at, join(&base, &name), dots == 0, true)),
            }
        }
    }
    // The two patterns' rows in the order the file writes them.
    found.sort_by_key(|(at, ..)| *at);
    for (_, target, near, up) in found {
        push(out, target, near, usize::from(up));
    }
}

// -------------------------------------------------------------------------------------- Script

/// The module after `from`, `import`, `require(` and `import(`, where it starts with `./` or
/// `../`: a package's name is no file of the vault.
fn script(rel: &str, masked: &str, text: &str, out: &mut Vec<Import>) {
    static KEYWORD: LazyLock<Regex> = LazyLock::new(|| regex(r"\b(from|import|require)\b"));
    for c in KEYWORD.captures_iter(masked) {
        let m = c.get(0).expect("the match");
        if masked[..m.start()].trim_end().ends_with('.') {
            continue;
        }
        let Some(spec) = literal(text, m.end(), &c[1] != "from", false) else {
            continue;
        };
        if spec.starts_with("./") || spec.starts_with("../") {
            let target = path::resolve(path::parent_dir(rel), spec);
            let target = target
                .strip_suffix("/index")
                .map(str::to_string)
                .unwrap_or(target);
            push(out, target, false, 0);
        }
    }
}

// ------------------------------------------------------------------------------------------- C

/// `#include "x"` and `#include <x>` alike, as written, near: a header is found by its path's
/// end, whichever folder the build adds to the search.
fn c(masked: &str, text: &str, out: &mut Vec<Import>) {
    static INCLUDE: LazyLock<Regex> = LazyLock::new(|| regex(r"(?m)^[ \t]*#[ \t]*include\b"));
    for m in INCLUDE.find_iter(masked) {
        if let Some(header) = literal(text, m.end(), false, true) {
            push(out, header.to_string(), true, 0);
        }
    }
}

// ------------------------------------------------------------------------------------------ Go

/// `import "p"`, `import name "p"` and the lines of an `import ( … )` block, each a package path
/// matched against folders.
fn go(masked: &str, text: &str, out: &mut Vec<Import>) {
    static IMPORT: LazyLock<Regex> = LazyLock::new(|| regex(r"(?m)^[ \t]*import\b"));
    static LINE: LazyLock<Regex> = LazyLock::new(|| regex(r#"^\s*(?:[\w.]+\s+)?"([^"]+)""#));
    for m in IMPORT.find_iter(masked) {
        // A blanked string is spaces too, so the block's `(` is looked for past them.
        let (from, rest) = (m.end(), &masked[m.end()..]);
        let end = match rest.trim_start().starts_with('(') {
            true => rest.find(')'),
            false => rest.find('\n'),
        };
        let lines = &text[from..end.map_or(text.len(), |e| from + e)];
        for line in lines.lines() {
            let line = line.split("//").next().unwrap_or_default();
            if let Some(p) = LINE.captures(line) {
                push(out, p[1].to_string(), true, 0);
            }
        }
    }
}

// ----------------------------------------------------------------------------- Java and Kotlin

/// `import [static] a.b.C [as D]`, near, its last name maybe a member of the class above; a
/// wildcard `a.b.*` names no file.
fn java(masked: &str, out: &mut Vec<Import>) {
    static IMPORT: LazyLock<Regex> = LazyLock::new(|| {
        regex(r"(?m)^[ \t]*import[ \t]+(?:static[ \t]+)?([\w.]+?)(\.\*)?(?:[ \t;]|$)")
    });
    for c in IMPORT.captures_iter(masked) {
        if c.get(2).is_none() {
            push(out, c[1].replace('.', "/"), true, 1);
        }
    }
}

// ---------------------------------------------------------------------------------------- Lean

/// `import A.B C.D` at the start of a line, each near.
fn lean(masked: &str, out: &mut Vec<Import>) {
    static IMPORT: LazyLock<Regex> = LazyLock::new(|| regex(r"(?m)^import[ \t]+([^\n]*)"));
    for c in IMPORT.captures_iter(masked) {
        for module in c[1].split_whitespace() {
            push(out, module.replace('.', "/"), true, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(target, near, key, up_key)` of what `text` imports from `rel`.
    fn read(lang: Lang, rel: &str, text: &str) -> Vec<(String, bool, String, Option<String>)> {
        let masked = super::super::mask(lang, text);
        let rows = imports(lang, rel, &masked, text).into_iter();
        rows.map(|i| (i.target, i.near, i.key, i.up_key)).collect()
    }

    fn targets(lang: Lang, rel: &str, text: &str) -> Vec<String> {
        read(lang, rel, text).into_iter().map(|row| row.0).collect()
    }

    /// `(target, up_key, up2_key, krate)` of what the Rust `text` imports from `rel`.
    type RustRow = (String, Option<String>, Option<String>, Option<String>);
    fn rust_rows(rel: &str, text: &str) -> Vec<RustRow> {
        let masked = super::super::mask(Lang::Rust, text);
        let rows = imports(Lang::Rust, rel, &masked, text).into_iter();
        rows.map(|i| {
            assert_eq!(Some(&i.key), module_key(&i.target).as_ref());
            (i.target, i.up_key, i.up2_key, i.krate)
        })
        .collect()
    }

    fn rust_row(target: &str, up: Option<&str>, up2: Option<&str>, krate: Option<&str>) -> RustRow {
        let owned = |s: Option<&str>| s.map(str::to_string);
        (target.to_string(), owned(up), owned(up2), owned(krate))
    }

    #[test]
    fn rust_modules_and_the_uses_of_this_crate() {
        let text = r#"
mod a;
#[cfg(test)]
pub(crate) mod tests;
use std::collections::HashMap;
use accent_api::Vault;
use crate::{Error, Result};
use crate::index::{Index, links::{self, ROW}};
use super::reconcile::link_kind_of;
pub use self::b::Thing as Other;
// use crate::commented::out;
const S: &str = "use crate::in_a_string;";
mod inline {
    mod nested;
    use super::sibling;
    use super::super::up;
}
"#;
        let row = |t: &str, up, up2| rust_row(t, up, up2, None);
        assert_eq!(
            rust_rows("crates/core/src/index/links.rs", text),
            [
                row("crates/core/src/index/links/a", None, None),
                row("crates/core/src/index/links/tests", None, None),
                row("crates/core/src/index/links/inline/nested", None, None),
                rust_row("src", None, None, Some("accent_api")),
                row("crates/core/src/index", Some("src"), None),
                row("crates/core/src/index/links", Some("index"), Some("src")),
                row(
                    "crates/core/src/index/reconcile/link_kind_of",
                    Some("reconcile"),
                    Some("index")
                ),
                row("crates/core/src/index/links/b", Some("links"), None),
                row("crates/core/src/index/up", Some("index"), None),
            ]
        );
    }

    #[test]
    fn rust_modules_at_a_path_declared_ones_and_other_crates() {
        let text = r#"
#[cfg(unix)]
#[path = "../platform/unix.rs"]
mod platform;
mod index;
use index::Index;
use platform::open;
use accent_core::index::links::Kind;
use log::debug;
use super::Item;
"#;
        assert_eq!(
            rust_rows("crates/api/src/local/mod.rs", text),
            [
                rust_row("crates/api/src/platform/unix", None, None, None),
                rust_row("crates/api/src/local/index", None, None, None),
                rust_row("crates/api/src/local/index", Some("local"), None, None),
                rust_row(
                    "crates/api/src/local/platform/open",
                    Some("platform"),
                    Some("local"),
                    None
                ),
                rust_row(
                    "src/index/links",
                    Some("index"),
                    Some("src"),
                    Some("accent_core")
                ),
                rust_row("src/debug", Some("src"), None, Some("log")),
            ]
        );
    }

    #[test]
    fn super_items_name_the_module_above_but_the_crate_root() {
        let text = "use super::Item;\nuse super::*;\n";
        // `super` is the crate root here, which every module uses, as `crate::Item` is.
        assert_eq!(rust_rows("crates/api/src/local/mod.rs", text), []);
        assert_eq!(rust_rows("crates/api/src/lib.rs", "use self::Item;\n"), []);
        assert_eq!(
            rust_rows("crates/api/src/local/files.rs", text),
            [rust_row("crates/api/src/local", None, None, None)]
        );
    }

    #[test]
    fn a_manifest_names_its_library_crate_as_a_use_does() {
        let package = "[package]\nname = \"accent-core\"\nversion = \"1.0.0\"\n";
        assert_eq!(crate_name(package).as_deref(), Some("accent_core"));
        let lib = "[package]\nname = \"accent-android\"\n\n[lib]\nname = \"ffi\"\n";
        assert_eq!(crate_name(lib).as_deref(), Some("ffi"));
        assert_eq!(crate_name("[workspace]\nmembers = [\"a\"]\n"), None);
        assert_eq!(crate_name("not toml ["), None);
    }

    #[test]
    fn python_imports_and_froms_relative_and_absolute() {
        let text = "import os.path, json as j\nfrom . import sibling\nfrom ..pkg.mod import (\n    a,  # one\n    b as c,\n)\nfrom lib import *\n";
        assert_eq!(
            read(Lang::Python, "proj/app/main.py", text),
            [
                ("os/path".into(), true, "path".into(), None),
                ("json".into(), true, "json".into(), None),
                (
                    "proj/app/sibling".into(),
                    false,
                    "sibling".into(),
                    Some("app".into())
                ),
                (
                    "proj/pkg/mod/a".into(),
                    false,
                    "a".into(),
                    Some("mod".into())
                ),
                (
                    "proj/pkg/mod/b".into(),
                    false,
                    "b".into(),
                    Some("mod".into())
                ),
                ("lib".into(), true, "lib".into(), None),
            ]
        );
    }

    #[test]
    fn scripts_import_their_relative_modules_only() {
        let text = "import x from './x.js';\nimport type { T } from \"../types\";\nexport * from './lib/index';\nconst y = require('./y');\nconst z = await import(\"./z\");\nimport React from 'react';\nfoo.require('./not');\n// import a from './commented';\n";
        assert_eq!(
            targets(Lang::Script, "web/src/app.ts", text),
            [
                "web/src/x.js",
                "web/types",
                "web/src/lib",
                "web/src/y",
                "web/src/z"
            ]
        );
    }

    #[test]
    fn c_go_java_lean_and_typst_imports() {
        let c = "#include \"util/str.h\"\n#include <stdio.h>\n// #include \"gone.h\"\n";
        assert_eq!(targets(Lang::C, "src/a.c", c), ["util/str.h", "stdio.h"]);
        let go = "import \"fmt\"\nimport (\n\tf \"example.com/me/proj/pkg/util\"\n\t\"example.com/me/proj/internal\" // why\n)\n";
        assert_eq!(
            targets(Lang::Go, "proj/main.go", go),
            [
                "fmt",
                "example.com/me/proj/pkg/util",
                "example.com/me/proj/internal"
            ]
        );
        let java = "import java.util.List;\nimport static org.x.Util.max;\nimport org.y.*;\n";
        let rows = read(Lang::Java, "src/A.java", java);
        assert_eq!(
            rows[0],
            (
                "java/util/List".into(),
                true,
                "list".into(),
                Some("util".into())
            )
        );
        assert_eq!(rows[1].0, "org/x/Util/max");
        assert_eq!(rows.len(), 2);
        let kotlin = "import a.b.C as D\n";
        assert_eq!(targets(Lang::Kotlin, "A.kt", kotlin), ["a/b/C"]);
        let lean = "import Mathlib.Data.Nat Accent.Basic\n-- import Gone\n";
        assert_eq!(
            targets(Lang::Lean, "A.lean", lean),
            ["Mathlib/Data/Nat", "Accent/Basic"]
        );
        let typst =
            "#include \"ch/one.typ\"\n#import \"/lib.typ\": f\n#import \"@preview/x:0.1.0\"\n";
        let rows = read(Lang::Typst, "book/main.typ", typst);
        let rows: Vec<_> = rows.into_iter().map(|row| (row.0, row.1)).collect();
        assert_eq!(
            rows,
            [("book/ch/one.typ".into(), false), ("lib.typ".into(), true)]
        );
    }

    #[test]
    fn a_folder_file_takes_its_folders_key() {
        assert_eq!(module_key("a/Util/mod.rs").as_deref(), Some("util"));
        assert_eq!(module_key("a/pkg/__init__.py").as_deref(), Some("pkg"));
        assert_eq!(module_key("a/lib/index.tsx").as_deref(), Some("lib"));
        assert_eq!(module_key("a/net/http.go").as_deref(), Some("net"));
        assert_eq!(module_key("a/foo.d.ts").as_deref(), Some("foo"));
        assert_eq!(module_key(".eslintrc.js"), None);
    }

    #[test]
    fn rank_puts_each_import_to_the_file_it_means() {
        let r = |src, target, near, up: bool, cand| rank(src, target, near, usize::from(up), cand);
        // Rust: a module's own file before its `mod.rs`, and an item's module above it.
        assert_eq!(r("src/lib.rs", "src/a", false, false, "src/a.rs"), Some(0));
        assert_eq!(
            r("src/lib.rs", "src/a", false, false, "src/a/mod.rs"),
            Some(1000)
        );
        assert_eq!(
            r(
                "src/x/y.rs",
                "src/x/reconcile/f",
                false,
                true,
                "src/x/reconcile.rs"
            ),
            Some(100_000)
        );
        assert_eq!(
            r("src/lib.rs", "src/a", false, false, "other/src/a.rs"),
            None
        );
        // Python: a package's `__init__.py`, exact for a relative import.
        assert_eq!(
            r("p/m.py", "p/sub", false, true, "p/sub/__init__.py"),
            Some(2000)
        );
        assert_eq!(r("p/m.py", "p/sub", false, true, "q/p/sub.py"), None);
        // TypeScript: `./x.js` is the `x.ts` it compiles from.
        assert!(r("web/a.ts", "web/x.js", false, false, "web/x.ts").is_some());
        // Near: the importer's own project's `utils.py` before another's.
        let mine = r("one/app/main.py", "utils", true, false, "one/utils.py").unwrap();
        let theirs = r("one/app/main.py", "utils", true, false, "two/deep/utils.py").unwrap();
        assert!(mine < theirs, "{mine} {theirs}");
        assert_eq!(r("one/a.py", "utils", true, false, "one/autils.py"), None);
        // Go: a package by at least its last two folders, the more the better.
        let near = r(
            "proj/main.go",
            "example.com/me/proj/pkg/util",
            true,
            false,
            "proj/pkg/util/a.go",
        );
        let far = r(
            "proj/main.go",
            "example.com/me/proj/pkg/util",
            true,
            false,
            "x/pkg/util/a.go",
        );
        assert!(near.unwrap() < far.unwrap());
        assert_eq!(
            r("proj/main.go", "fmt", true, false, "vendor/fmt/print.go"),
            None
        );
        assert_eq!(
            r(
                "proj/main.go",
                "a/pkg/util",
                true,
                false,
                "pkg/util/a_test.go"
            ),
            None
        );
    }
}
