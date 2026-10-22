// Derived from formulary 0.3.0 `src/parse.rs` (MIT OR Apache-2.0, Copyright (c) 2026 Jeffrey T.
// Peckham (Ophymx)), modified for accent; see crates/math/NOTICE.
//! MathML to the element tree, for the elements and attributes pulldown-latex 0.8 writes. Its
//! `class` and `style` attributes are read as its `styles.css` and inline CSS mean them; the
//! line numbers below are in its `src/mathml.rs`.
//!
//! Lenient as MathML Core is: an unknown element, or a known one with the wrong children, lays
//! out as an `mrow` of its children and adds a warning. Only malformed XML and a root other than
//! `<math>` are errors.

use crate::Error;
use crate::ast::{
    Align, Cell, Color, Length, MathRoot, Node, OperatorAttrs, ScriptLevel, StyleOverrides,
    TableRow,
};
use crate::mathvariant::to_math_italic;

/// The marks `\hat`, `\vec`, `\overbrace` and the like set over their argument
/// (`parser/primitives.rs:635-690`), written as a plain `<mover>`. `\Overrightarrow`'s ⇒ has no
/// combining form to be drawn as an accent in, and stays an overscript.
const OVER_ACCENTS: &str = "´‾˘ˇ˙¨`^~→˚←↔↼⇀⏜⏠⏞⎴";
/// The marks `\underline`, `\underbrace` and the like set under theirs; ↔ stays an underscript.
const UNDER_ACCENTS: &str = "_←→⏟⏝⎵⏡";

/// Parses a MathML fragment whose root element is `<math>`.
pub fn parse(source: &str) -> Result<MathRoot, Error> {
    let doc = roxmltree::Document::parse(source)?;
    let root = doc.root_element();
    if root.tag_name().name() != "math" {
        return Err(Error::NotMath(root.tag_name().name().to_string()));
    }
    let mut warnings = Vec::new();
    let children = parse_children(root, &mut warnings);
    Ok(MathRoot {
        display: root.attribute("display") == Some("block"),
        children,
        warnings,
    })
}

fn parse_children(parent: roxmltree::Node, warnings: &mut Vec<String>) -> Vec<Node> {
    parent
        .children()
        .filter(|c| c.is_element())
        .map(|c| parse_node(c, warnings))
        .collect()
}

/// One element, inside a style scope if it sets a style.
fn parse_node(node: roxmltree::Node, warnings: &mut Vec<String>) -> Node {
    let parsed = parse_element(node, warnings);
    let styles = style_overrides(node);
    if styles.is_empty() {
        parsed
    } else {
        Node::Styled {
            styles,
            children: vec![parsed],
        }
    }
}

/// The style an element sets: `displaystyle` and `scriptlevel` (88-92), the colours `open_tag`
/// writes into `style` (106-125), `class="small"`, 70 % (746, 757), and the stroke of `\not`
/// before a group (`mop-negated`, 413).
fn style_overrides(node: roxmltree::Node) -> StyleOverrides {
    let mut styles = StyleOverrides {
        display_style: bool_attr(node, "displaystyle"),
        script_level: node.attribute("scriptlevel").and_then(parse_script_level),
        size: has_class(node, "small").then_some(0.7),
        strike: has_class(node, "mop-negated"),
        ..StyleOverrides::default()
    };
    for (property, value) in css(node) {
        match property {
            "color" => styles.color = parse_rgb(value),
            "background-color" => styles.background = parse_rgb(value),
            // Always `0.06em solid rgb(…)`: the colour is what varies.
            "border" => styles.border = value.find("rgb(").and_then(|i| parse_rgb(&value[i..])),
            _ => {}
        }
    }
    styles
}

fn parse_element(node: roxmltree::Node, warnings: &mut Vec<String>) -> Node {
    let name = node.tag_name().name();
    match name {
        "mi" => {
            let text = text_content(node);
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                // A single character is italic unless it says otherwise: pulldown-latex writes
                // `mathvariant="normal"` for upright symbols and maps every other style itself.
                (Some(c), None) if node.attribute("mathvariant") != Some("normal") => {
                    Node::Identifier(to_math_italic(c).to_string())
                }
                _ => Node::Identifier(text),
            }
        }
        "mn" => Node::Number(text_content(node)),
        "mo" => Node::Operator {
            text: text_content(node),
            attrs: OperatorAttrs {
                stretchy: bool_attr(node, "stretchy"),
                symmetric: bool_attr(node, "symmetric"),
                largeop: bool_attr(node, "largeop"),
                movablelimits: bool_attr(node, "movablelimits"),
                minsize: length_attr(node, "minsize"),
                maxsize: length_attr(node, "maxsize"),
            },
        },
        "mtext" => Node::Text(text_content(node)),
        "mrow" => Node::Row(parse_children(node, warnings)),
        "mfrac" => match <[Node; 2]>::try_from(parse_children(node, warnings)) {
            Ok([num, den]) => Node::Frac {
                num: Box::new(num),
                den: Box::new(den),
                line_thickness: length_attr(node, "linethickness"),
            },
            Err(children) => invalid(name, children, warnings),
        },
        "msub" | "msup" | "msubsup" | "munder" | "mover" | "munderover" => {
            let mut children = parse_children(node, warnings);
            let arity = if matches!(name, "msubsup" | "munderover") {
                3
            } else {
                2
            };
            // The thin space before a function name goes inside the script element the name is
            // the base of (642-648): `2\sin^2 x` gives `<msup><mspace/><mi>sin</mi>…`.
            let space = (children.len() == arity + 1 && matches!(children[0], Node::Space { .. }))
                .then(|| children.remove(0));
            if children.len() != arity {
                return invalid(name, children, warnings);
            }
            let scripted = scripts(node, name, children);
            match space {
                Some(space) => Node::Row(vec![space, scripted]),
                None => scripted,
            }
        }
        "mtable" => parse_table(node, warnings),
        "msqrt" => Node::Sqrt(parse_children(node, warnings)),
        "mroot" => match <[Node; 2]>::try_from(parse_children(node, warnings)) {
            Ok([base, index]) => Node::Root {
                base: Box::new(base),
                index: Box::new(index),
            },
            Err(children) => invalid(name, children, warnings),
        },
        "mspace" => Node::Space {
            width: length_attr(node, "width"),
            height: length_attr(node, "height"),
            depth: length_attr(node, "depth"),
        },
        other => {
            warnings.push(format!("unknown element <{other}>"));
            // An unknown element holding only text keeps the text.
            if node.children().any(|c| c.is_element()) {
                Node::Row(parse_children(node, warnings))
            } else {
                Node::Text(text_content(node))
            }
        }
    }
}

/// `<msub>`, `<msup>`, `<msubsup>`, `<munder>`, `<mover>` or `<munderover>` of `children`,
/// as many as it takes.
fn scripts(node: roxmltree::Node, name: &str, children: Vec<Node>) -> Node {
    let mut children = children.into_iter();
    let base = Box::new(children.next().expect("length checked"));
    let unsaid = |a| node.attribute(a).is_none();
    let (first, second) = match (name, children.next(), children.next()) {
        ("mover", Some(script), None) if unsaid("accent") => match accent(&script, OVER_ACCENTS) {
            Some('‾') => return Node::Bar { base, under: false },
            Some(c) => return accented(base, c, false, matches!(script, Node::Operator { .. })),
            None => (Some(script), None),
        },
        ("munder", Some(script), None) if unsaid("accentunder") => {
            match accent(&script, UNDER_ACCENTS) {
                Some('_') => return Node::Bar { base, under: true },
                Some(c) => return accented(base, c, true, matches!(script, Node::Operator { .. })),
                None => (Some(script), None),
            }
        }
        (_, first, second) => (first, second),
    };
    let (first, second) = (first.map(Box::new), second.map(Box::new));
    match name {
        "msub" => Node::Scripts {
            base,
            sub: first,
            sup: None,
        },
        "msup" => Node::Scripts {
            base,
            sub: None,
            sup: first,
        },
        "msubsup" => Node::Scripts {
            base,
            sub: first,
            sup: second,
        },
        _ => {
            let (under, over) = match name {
                "munder" => (first, None),
                "mover" => (None, first),
                _ => (first, second),
            };
            Node::UnderOver {
                base,
                under,
                over,
                accent: bool_attr(node, "accent").unwrap_or(false),
                accent_under: bool_attr(node, "accentunder").unwrap_or(false),
            }
        }
    }
}

/// The accent mark `script` is, if it is a lone character from `marks` in an `<mi>` or `<mo>`:
/// pulldown-latex gives the `<mover>` or `<munder>` no `accent` attribute (2086-2114).
fn accent(script: &Node, marks: &str) -> Option<char> {
    let (Node::Identifier(text) | Node::Operator { text, .. }) = script else {
        return None;
    };
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if marks.contains(c) => Some(c),
        _ => None,
    }
}

/// `base` with the accent `mark` over or under it. A spacing mark or an arrow becomes its
/// combining form: that is what a math font draws to sit on a base and has wide variants of,
/// where its arrows sit on the math axis. Only a mark that came as an `<mo>` (`\widehat`,
/// `\overrightarrow`) stretches across the base; one that came as an `<mi>` (`\hat`, `\vec`)
/// keeps its natural size, as in TeX.
fn accented(base: Box<Node>, mark: char, under: bool, stretchy: bool) -> Node {
    let mark = match (mark, under) {
        ('^', _) => '\u{302}',
        ('~', _) => '\u{303}',
        ('ˇ', _) => '\u{30C}',
        ('´', _) => '\u{301}',
        ('`', _) => '\u{300}',
        ('˘', _) => '\u{306}',
        ('˙', _) => '\u{307}',
        ('¨', _) => '\u{308}',
        ('˚', _) => '\u{30A}',
        ('→', false) => '\u{20D7}',
        ('←', false) => '\u{20D6}',
        ('↔', false) => '\u{20E1}',
        ('⇀', false) => '\u{20D1}',
        ('↼', false) => '\u{20D0}',
        ('→', true) => '\u{20EF}',
        ('←', true) => '\u{20EE}',
        (c, _) => c,
    };
    let script = Some(Box::new(Node::Operator {
        text: mark.to_string(),
        attrs: OperatorAttrs {
            stretchy: Some(stretchy),
            ..OperatorAttrs::default()
        },
    }));
    let (over, under) = if under {
        (None, script)
    } else {
        (script, None)
    };
    Node::UnderOver {
        accent: over.is_some(),
        accent_under: under.is_some(),
        base,
        under,
        over,
    }
}

/// An `<mtable>` with pulldown-latex's classes (196-389, 1021-1165) as its `styles.css` has
/// them. A cell has no padding beside it but 0.4 em in an array or matrix
/// (`menv-arraylike`); `align` puts 1 em before columns 3, 5, … and `cases` after column 1.
/// Cells align right then left in turn in `align`-like tables, as a whole table says
/// (`menv-cells-left`) or as a cell says (`cell-left`). An array's vertical lines are cell
/// borders, dashed ones drawn solid, a line beside a line a cell of its own 0.1 em wide
/// (`menv-border-only`); `\hline` is a row border, and a doubled one has a row between for room
/// (`menv-nonumber`). Equation numbers (`menv-with-eqn`) are not drawn: draw.io's and Jupyter's
/// MathJax leave them out (`tags: 'none'`).
fn parse_table(node: roxmltree::Node, warnings: &mut Vec<String>) -> Node {
    let class = |c| has_class(node, c);
    let (alignlike, align_env, cases) = (
        class("menv-alignlike"),
        class("menv-align"),
        class("menv-cases"),
    );
    let pad = if class("menv-arraylike") { 0.4 } else { 0.0 };
    let all = if class("menv-cells-left") {
        Some(Align::Left)
    } else if class("menv-cells-right") {
        Some(Align::Right)
    } else {
        None
    };
    let plain = |content| Cell {
        content,
        align: Align::Center,
        pad: (pad, pad),
        rules: (false, false),
    };
    let rows = node.children().filter(|c| c.is_element()).map(|row| {
        if row.tag_name().name() != "mtr" {
            // Wrapped as browsers do: a stray child is a row of one cell.
            warnings.push(format!("<{}> in <mtable>", row.tag_name().name()));
            return TableRow {
                cells: vec![plain(parse_node(row, warnings))],
                rule_above: false,
                min_height: None,
                spacer: false,
            };
        }
        let mut elements = row.children().filter(|c| c.is_element());
        let spacer = matches!(
            (elements.next(), elements.next()),
            (Some(cell), None) if has_class(cell, "menv-nonumber")
        );
        let cells = row
            .children()
            .filter(|c| c.is_element())
            .enumerate()
            .map(|(j, cell)| {
                if cell.tag_name().name() != "mtd" {
                    warnings.push(format!("<{}> in <mtr>", cell.tag_name().name()));
                    return plain(parse_node(cell, warnings));
                }
                let class = |c| has_class(cell, c);
                let align = if class("cell-left") {
                    Align::Left
                } else if class("cell-right") {
                    Align::Right
                } else if let Some(all) = all {
                    all
                } else if alignlike {
                    // nth-child(odd) right, nth-child(even) left.
                    if j % 2 == 0 {
                        Align::Right
                    } else {
                        Align::Left
                    }
                } else {
                    Align::Center
                };
                let pad = if class("menv-border-only") {
                    (0.05, 0.05)
                } else {
                    let left = if align_env && j >= 2 && j % 2 == 0 {
                        1.0
                    } else {
                        pad
                    };
                    let right = if cases && j == 0 { 1.0 } else { pad };
                    (left, right)
                };
                Cell {
                    content: Node::Row(parse_children(cell, warnings)),
                    align,
                    pad,
                    rules: (
                        class("menv-left-solid") || class("menv-left-dashed"),
                        class("menv-right-solid") || class("menv-right-dashed"),
                    ),
                }
            });
        TableRow {
            cells: cells.collect(),
            rule_above: has_class(row, "menv-hline") || has_class(row, "menv-hdashline"),
            min_height: css(row)
                .find(|(p, _)| *p == "height")
                .and_then(|(_, v)| parse_length(v)),
            spacer,
        }
    });
    let mut rows: Vec<TableRow> = rows.collect();
    // A last row of one empty cell is not shown (`styles.css`): what a trailing `\\` leaves.
    if let Some(last) = rows.last_mut()
        && matches!(last.cells.as_slice(), [Cell { content: Node::Row(c), .. }] if c.is_empty())
    {
        last.cells.clear();
    }
    // `multline` sets its first line to the left, its last to the right and any between in the
    // middle, as TeX does, here within the formula's own width. `styles.css` means the same, but
    // its `mtd:first-child` and `mtd:last-child` both match a line's one cell.
    if class("menv-multline") {
        let mut lines: Vec<_> = rows
            .iter_mut()
            .filter(|r| !r.spacer && !r.cells.is_empty())
            .collect();
        let last = lines.len().saturating_sub(1);
        for (i, line) in lines.iter_mut().enumerate() {
            let align = match i {
                _ if last == 0 => Align::Center,
                0 => Align::Left,
                i if i == last => Align::Right,
                _ => Align::Center,
            };
            line.cells.iter_mut().for_each(|c| c.align = align);
        }
    }
    Node::Table(rows)
}

fn has_class(node: roxmltree::Node, class: &str) -> bool {
    node.attribute("class")
        .is_some_and(|c| c.split_whitespace().any(|c| c == class))
}

/// The declarations of an element's `style` attribute.
fn css<'a>(node: roxmltree::Node<'a, '_>) -> impl Iterator<Item = (&'a str, &'a str)> {
    node.attribute("style")
        .unwrap_or("")
        .split(';')
        .filter_map(|d| d.split_once(':'))
        .map(|(p, v)| (p.trim(), v.trim()))
}

/// `rgb(r g b)`, the one colour form pulldown-latex writes.
fn parse_rgb(s: &str) -> Option<Color> {
    let inner = s.trim().strip_prefix("rgb(")?.strip_suffix(')')?;
    let channels: Vec<u8> = inner
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let [r, g, b] = channels[..] else {
        return None;
    };
    Some(Color { r, g, b, a: 255 })
}

/// The `mrow` a fixed-arity element with the wrong number of children lays out as.
fn invalid(element: &str, children: Vec<Node>, warnings: &mut Vec<String>) -> Node {
    warnings.push(format!("<{element}> with {} children", children.len()));
    Node::Row(children)
}

/// A length attribute. An invalid value is taken as no attribute, MathML's recovery.
fn length_attr(node: roxmltree::Node, name: &str) -> Option<Length> {
    node.attribute(name).and_then(parse_length)
}

/// A boolean attribute, ASCII case-insensitive as MathML Core has it.
fn bool_attr(node: roxmltree::Node, name: &str) -> Option<bool> {
    match node.attribute(name) {
        Some(v) if v.eq_ignore_ascii_case("true") => Some(true),
        Some(v) if v.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

fn parse_length(s: &str) -> Option<Length> {
    let s = s.trim();
    let split = s
        .find(|c: char| c != '+' && c != '-' && c != '.' && !c.is_ascii_digit())
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let value: f32 = num.parse().ok()?;
    match unit.trim() {
        "em" => Some(Length::Em(value)),
        // pulldown-latex converts mu to em and writes the unit as mu (`event.rs:762-764`).
        "mu" => Some(Length::Em(value)),
        "ex" => Some(Length::Ex(value)),
        "px" => Some(Length::Px(value)),
        "pt" => Some(Length::Px(value * 96.0 / 72.0)),
        "pc" => Some(Length::Px(value * 16.0)),
        "in" => Some(Length::Px(value * 96.0)),
        "cm" => Some(Length::Px(value * 96.0 / 2.54)),
        "mm" => Some(Length::Px(value * 96.0 / 25.4)),
        // A unitless number other than zero is invalid in MathML Core.
        "" if value == 0.0 => Some(Length::Px(0.0)),
        _ => None,
    }
}

fn parse_script_level(s: &str) -> Option<ScriptLevel> {
    let s = s.trim();
    if s.starts_with('+') || s.starts_with('-') {
        s.parse().ok().map(ScriptLevel::Add)
    } else {
        s.parse().ok().map(ScriptLevel::Set)
    }
}

/// A token element's text, trimmed as MathML trims it: no whitespace at either end, and a run
/// inside collapsed to one space. Only XML whitespace collapses; U+00A0 and the other Unicode
/// spaces are content.
fn text_content(node: roxmltree::Node) -> String {
    let raw: String = node.children().filter_map(|c| c.text()).collect();
    let mut out = String::with_capacity(raw.len());
    let mut in_space = true;
    for c in raw.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\r') {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(c);
            in_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}
