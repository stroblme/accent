//! A formula's TeX with the commands that give their argument a class rewritten where
//! pulldown-latex 0.8 misreads them (ISSUES.md), into forms it reads as MathJax sets them.
//!
//! pulldown-latex takes the argument of `\mathop` and `\operatorname` for the operator's name,
//! so `\mathop{\times}` showed `\times`, and it gives `\mathbin`, `\mathrel` and the like their
//! class only for a single character, not for a command such as `\times`. It knows neither
//! `\operatorname*` nor `\DeclareMathOperator`, and takes `\limits` only after some functions.

use pulldown_latex::event::{Content, Event};
use pulldown_latex::{Parser, Storage};
use std::borrow::Cow;

/// The class commands pulldown-latex reads right when their argument is one character.
const CLASSES: [&str; 7] = [
    "mathbin",
    "mathrel",
    "mathord",
    "mathopen",
    "mathclose",
    "mathpunct",
    "mathinner",
];

/// `tex` with every class command pulldown-latex would misread rewritten.
pub(super) fn atoms(tex: &str) -> Cow<'_, str> {
    let commands = [
        "\\math",
        "\\operatorname",
        "\\DeclareMathOperator",
        "limits",
    ];
    if commands.iter().any(|c| tex.contains(c)) {
        Cow::Owned(rewrite(tex))
    } else {
        Cow::Borrowed(tex)
    }
}

fn rewrite(tex: &str) -> String {
    let mut out = String::with_capacity(tex.len());
    let mut rest = tex;
    while let Some(i) = rest.find('\\') {
        out.push_str(&rest[..i]);
        let (cs, after) = control_sequence(&rest[i..]);
        rest = match command(&cs[1..], after) {
            Some((rewritten, after)) => {
                out.push_str(&rewritten);
                after
            }
            None => {
                out.push_str(cs);
                after
            }
        };
    }
    out.push_str(rest);
    out
}

/// The command `\name` and what follows it rewritten, or `None` where pulldown-latex reads it
/// right as it is.
fn command<'a>(name: &str, after: &'a str) -> Option<(String, &'a str)> {
    match name {
        "mathop" => {
            let (arg, rest) = argument(after)?;
            Some((op(arg, "mathord", ""), rest))
        }
        "operatorname" => {
            let (star, after) = star(after);
            let (arg, rest) = argument(after)?;
            Some((operatorname(arg, star), rest))
        }
        // Defined for this formula only: MathJax keeps it for the page.
        "DeclareMathOperator" => {
            let (star, after) = star(after);
            let (cs, after) = argument(after)?;
            let (arg, rest) = argument(after)?;
            let body = operatorname(arg, star);
            // pulldown-latex wants the count of arguments, which TeX does not.
            Some((format!("\\newcommand{{{cs}}}[0]{{{body}}}"), rest))
        }
        _ if CLASSES.contains(&name) => {
            let (arg, rest) = argument(after)?;
            Some((format!("\\{name}{{{}}}", symbol(arg)?), rest))
        }
        // `\sin\limits`: pulldown-latex takes `\limits` after `\mathop` alone of the functions.
        _ => {
            let next = after.trim_start();
            let modifier = next.starts_with('\\').then(|| control_sequence(next).0);
            if !matches!(modifier, Some("\\limits" | "\\nolimits")) {
                return None;
            }
            let function = content(&format!("\\{name}"), |c| match c {
                Content::Function(f) => Some(f.to_string()),
                _ => None,
            })?;
            Some((format!("\\mathop{{{function}}}"), after))
        }
    }
}

/// `\operatorname`'s argument set upright, as MathJax sets it, with its limits beside it, or
/// above and below in display where starred.
fn operatorname(arg: &str, star: bool) -> String {
    op(arg, "mathrm", if star { "" } else { "\\nolimits" })
}

/// An operator of `arg` that pulldown-latex reads right, followed by `limits`. A symbol is its
/// character, made the operator itself; a name, plain or upright, is set as `\sin` is; anything
/// else is a group in `font`, whose limits `\mathop` places as it does a symbol's.
fn op(arg: &str, font: &str, limits: &str) -> String {
    let arg = arg.trim();
    if let Some(c) = symbol(arg) {
        format!("\\mathop{{{c}}}{limits}")
    } else if let Some(name) = upright(arg).or(plain(arg).then_some(arg)) {
        format!("\\mathop{{{name}}}{limits}")
    } else {
        format!("\\mathop\\{font}{{{}}}{limits}", rewrite(arg))
    }
}

/// The one symbol `arg` is, such as `×` for `\times`, though not a letter, which keeps its
/// look as the group pulldown-latex makes of it; a brace or another character special to TeX
/// cannot stand in for its command.
fn symbol(arg: &str) -> Option<char> {
    let c = content(arg, |c| match *c {
        Content::BinaryOp { content, .. }
        | Content::LargeOp { content, .. }
        | Content::Delimiter { content, .. }
        | Content::Punctuation(content) => Some(content),
        Content::Ordinary { content, .. } if !content.is_alphanumeric() => Some(content),
        Content::Relation { content, .. } => {
            let mut buf = [0; 8];
            let text = std::str::from_utf8(content.encode_utf8_to_buf(&mut buf)).ok()?;
            let mut chars = text.chars();
            let c = chars.next()?;
            chars.next().is_none().then_some(c)
        }
        _ => None,
    })?;
    (!"{}%&#_^~\\$".contains(c)).then_some(c)
}

/// The name `\rm name` or `\mathrm{name}` sets upright.
fn upright(arg: &str) -> Option<&str> {
    let name = match arg.strip_prefix("\\rm") {
        Some(name) if name.starts_with(char::is_whitespace) => name.trim(),
        _ => arg.strip_prefix("\\mathrm{")?.strip_suffix('}')?,
    };
    plain(name).then_some(name)
}

/// Whether `s` is a name pulldown-latex can set as written.
fn plain(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-')
}

/// What `read` makes of `tex`, where pulldown-latex reads it as one piece of content.
fn content<T>(tex: &str, read: impl FnOnce(&Content) -> Option<T>) -> Option<T> {
    let storage = Storage::new();
    let events = Parser::new(tex, &storage)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    match &events[..] {
        [Event::Content(c)] => read(c),
        _ => None,
    }
}

/// The control sequence `s` starts with, backslash included, and what follows it: the run of
/// letters after the backslash, or the one character that is not a letter.
fn control_sequence(s: &str) -> (&str, &str) {
    let name = &s[1..];
    let len = match name.find(|c: char| !c.is_ascii_alphabetic()) {
        Some(0) => name.chars().next().map_or(0, char::len_utf8),
        Some(n) => n,
        None => name.len(),
    };
    s.split_at(1 + len)
}

/// A command's argument, as TeX reads one after any spaces: a braced group's content, or a
/// single token; and what follows it. `None` at the end of a group or for an unclosed one.
fn argument(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    let Some(group) = s.strip_prefix('{') else {
        if s.starts_with('\\') {
            return Some(control_sequence(s));
        }
        let c = s.chars().next().filter(|&c| c != '}')?;
        return Some(s.split_at(c.len_utf8()));
    };
    let mut depth = 0;
    let mut chars = group.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '{' => depth += 1,
            '}' if depth == 0 => return Some((&group[..i], &group[i + 1..])),
            '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Whether a starred form follows, as `\operatorname*`, and what follows the star.
fn star(s: &str) -> (bool, &str) {
    match s.trim_start().strip_prefix('*') {
        Some(rest) => (true, rest),
        None => (false, s),
    }
}
