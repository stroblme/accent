//! A formula's TeX with the commands that give their argument a class rewritten where
//! pulldown-latex 0.8 misreads them (ISSUES.md), into forms it reads as MathJax sets them.
//!
//! pulldown-latex takes the argument of `\mathop` and `\operatorname` for the operator's name,
//! so `\mathop{\times}` showed `\times`, and it gives `\mathbin`, `\mathrel` and the like their
//! class only for a single character, not for a command such as `\times`. Even then its MathML
//! spaces an operator by the operator dictionary, not by its class, so a class command that
//! changes a symbol's class sets it as an ordinary atom with the space of the class around it.
//! It knows neither `\operatorname*` nor `\DeclareMathOperator`, and takes `\limits` only
//! after some functions.

use pulldown_latex::event::{Content, DelimiterType, Event, ScriptType, Visual};
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
        // Nothing before it in its group, or an opening delimiter: no atom to space it from.
        let first = matches!(
            out.trim_end().chars().last(),
            None | Some('{' | '&' | '(' | '[' | '\\')
        );
        let (cs, after) = control_sequence(&rest[i..]);
        rest = match command(&cs[1..], after, first) {
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
/// right as it is; `first` where nothing stands before it to space it from.
fn command<'a>(name: &str, after: &'a str, first: bool) -> Option<(String, &'a str)> {
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
            Some(class(name, arg, rest, first))
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
/// above and below in display where starred. A plain name without the star pulldown-latex reads
/// right itself.
fn operatorname(arg: &str, star: bool) -> String {
    let arg = arg.trim();
    if !star && plain(arg) {
        return format!("\\operatorname{{{arg}}}");
    }
    op(arg, "mathrm", if star { "" } else { "\\nolimits" })
}

/// An operator of `arg` that pulldown-latex reads right, followed by `limits`. A symbol is its
/// character, made the operator itself; a name, plain or upright, is set as `\sin` is; anything
/// else, a single letter too, which pulldown-latex would make an operator symbol, is a group in
/// `font`, whose limits `\mathop` places as it does a symbol's.
fn op(arg: &str, font: &str, limits: &str) -> String {
    let arg = arg.trim();
    if let Some((c, _)) = symbol(arg) {
        format!("\\mathop{{{c}}}{limits}")
    } else if let Some(name) = upright(arg)
        .or(plain(arg).then_some(arg))
        .filter(|name| name.chars().nth(1).is_some())
    {
        format!("\\mathop{{{name}}}{limits}")
    } else {
        format!("\\mathop\\{font}{{{}}}{limits}", rewrite(arg))
    }
}

/// The space TeX sets on either side of an atom of the class `\name` gives, between ordinary
/// atoms, as the commands pulldown-latex reads: a medium space round a binary operator, a thick
/// one round a relation, a thin one after punctuation and round an inner atom.
fn spaces(name: &str) -> (&'static str, &'static str) {
    match name {
        "mathbin" => ("\\:", "\\:"),
        "mathrel" => ("\\;", "\\;"),
        "mathpunct" => ("", "\\,"),
        "mathinner" => ("\\,", "\\,"),
        _ => ("", ""),
    }
}

/// `\name{arg}`, a class command but `\mathop`, and what follows it. A symbol of that class
/// already, or any symbol where the class takes no space, is that class's to pulldown-latex, and
/// one operator, as `\overset{!}{=}` is, is spaced as its core is. Anything else is an ordinary
/// atom with the space of the class on each side that has a neighbour, after its scripts: a run
/// of symbols set close, as TeX sets `:=`, or the group it is.
fn class<'a>(name: &str, arg: &str, rest: &'a str, first: bool) -> (String, &'a str) {
    let (before, after) = spaces(name);
    let pieces = pieces(arg).unwrap_or_default();
    match pieces[..] {
        [
            Piece {
                symbol: Some((c, class)),
                ..
            },
        ] if class == name || before.is_empty() && after.is_empty() => {
            return (format!("\\{name}{{{c}}}"), rest);
        }
        [
            Piece {
                symbol: None,
                operator: true,
            },
        ] => return (format!("\\{name}{{{}}}", rewrite(arg)), rest),
        _ if before.is_empty() && after.is_empty() => {
            return (format!("\\{name}{{{}}}", rewrite(arg)), rest);
        }
        _ => {}
    }
    let symbols: Option<String> = (pieces.iter())
        .map(|p| p.symbol.map(|(c, _)| format!("\\mathord{{{c}}}")))
        .collect();
    let atom = symbols
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| rewrite(arg));
    let (scripts, rest) = scripts(rest);
    let before = if first { "" } else { before };
    let after = if ends(rest) { "" } else { after };
    (format!("{before}\\mathord{{{atom}}}{scripts}{after}"), rest)
}

/// The scripts after an atom, their arguments rewritten, and what follows them.
fn scripts(mut rest: &str) -> (String, &str) {
    let mut out = String::new();
    loop {
        let next = rest.trim_start();
        let Some(mark) = next.chars().next().filter(|c| matches!(c, '_' | '^')) else {
            break;
        };
        let Some((arg, after)) = argument(&next[1..]) else {
            break;
        };
        out.push_str(&format!("{mark}{{{}}}", rewrite(arg)));
        rest = after;
    }
    (out, rest)
}

/// Whether `rest` holds nothing more of its group to space an atom from: the end of the group,
/// of a cell or line, or a closing delimiter.
fn ends(rest: &str) -> bool {
    let next = rest.trim_start();
    next.is_empty()
        || next.starts_with(['}', '&', ')', ']'])
        || ["\\\\", "\\right", "\\end"]
            .iter()
            .any(|e| next.starts_with(e))
}

/// One atom of what pulldown-latex reads: its character and the class command of its class
/// where it is a single symbol, and whether it is an operator, decorated or not, which MathML
/// spaces as that operator.
#[derive(Clone, Copy)]
struct Piece {
    symbol: Option<(char, &'static str)>,
    operator: bool,
}

/// The atoms of `tex` at its top level; `None` where it does not parse.
fn pieces(tex: &str) -> Option<Vec<Piece>> {
    let storage = Storage::new();
    let events: Vec<Event> = Parser::new(tex, &storage)
        .filter(|e| !matches!(e, Ok(Event::StateChange(_))))
        .collect::<Result<_, _>>()
        .ok()?;
    let mut rest = &events[..];
    let mut out = Vec::new();
    while !rest.is_empty() {
        let (p, after) = piece(rest)?;
        out.push(p);
        rest = after;
    }
    Some(out)
}

/// The first atom of `events`, and the events after it.
fn piece<'e, 'a>(events: &'e [Event<'a>]) -> Option<(Piece, &'e [Event<'a>])> {
    let plain = |operator| Piece {
        symbol: None,
        operator,
    };
    let (first, mut rest) = events.split_first()?;
    let piece = match first {
        Event::Content(c) => Piece {
            symbol: symbol_of(c),
            operator: !matches!(
                c,
                Content::Ordinary { .. }
                    | Content::Number(_)
                    | Content::Text(_)
                    | Content::Function(_)
            ),
        },
        // A base and its one or two scripts, decorating it.
        Event::Script { ty, .. } => {
            let (base, after) = piece(rest)?;
            rest = after;
            for _ in 0..if *ty == ScriptType::SubSuperscript {
                2
            } else {
                1
            } {
                rest = piece(rest)?.1;
            }
            plain(base.operator)
        }
        Event::Visual(visual) => {
            let (inner, after) = piece(rest)?;
            rest = after;
            if matches!(visual, Visual::Root | Visual::Fraction(_)) {
                rest = piece(rest)?.1;
            }
            plain(*visual == Visual::Negation && inner.operator)
        }
        Event::Begin(_) => {
            let mut inside = Vec::new();
            while !matches!(rest.first(), None | Some(Event::End)) {
                let (p, after) = piece(rest)?;
                inside.push(p);
                rest = after;
            }
            rest = rest.get(1..)?;
            plain(matches!(inside[..], [Piece { operator: true, .. }]))
        }
        _ => plain(false),
    };
    Some((piece, rest))
}

/// The one symbol `arg` is, such as `×` for `\times`, and the class command of its class.
fn symbol(arg: &str) -> Option<(char, &'static str)> {
    content(arg, symbol_of)
}

/// The symbol `c` is, and the class command of its class, though not a letter, which keeps its
/// look as the group pulldown-latex makes of it, nor a sized or small one such as `\big|`,
/// whose character is set at its natural size; a brace or another character special to TeX
/// cannot stand in for its command.
fn symbol_of(c: &Content) -> Option<(char, &'static str)> {
    let symbol = match *c {
        Content::BinaryOp {
            content,
            small: false,
        } => (content, "mathbin"),
        Content::LargeOp {
            content,
            small: false,
        } => (content, "mathop"),
        Content::Delimiter {
            content,
            size: None,
            ty,
        } => match ty {
            DelimiterType::Open => (content, "mathopen"),
            DelimiterType::Close => (content, "mathclose"),
            DelimiterType::Fence => (content, "mathord"),
        },
        Content::Punctuation(content) => (content, "mathpunct"),
        Content::Ordinary {
            content,
            stretchy: false,
        } if !content.is_alphanumeric() => (content, "mathord"),
        Content::Relation {
            content,
            small: false,
        } => {
            let mut buf = [0; 8];
            let text = std::str::from_utf8(content.encode_utf8_to_buf(&mut buf)).ok()?;
            let mut chars = text.chars();
            let c = chars.next()?;
            (chars.next().is_none().then_some(c)?, "mathrel")
        }
        _ => return None,
    };
    (!"{}%&#_^~\\$".contains(symbol.0)).then_some(symbol)
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
/// single token; and what follows it. `None` at the end of a group or for an unclosed one, and
/// for a macro's parameter, which only the macro's use fills in.
fn argument(s: &str) -> Option<(&str, &str)> {
    let (arg, rest) = token_or_group(s)?;
    (!arg.contains('#')).then_some((arg, rest))
}

fn token_or_group(s: &str) -> Option<(&str, &str)> {
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
