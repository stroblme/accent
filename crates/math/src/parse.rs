// Derived from formulary 0.3.0 `src/parse.rs` (MIT OR Apache-2.0, Copyright (c) 2026 Jeffrey T.
// Peckham (Ophymx)), modified for accent; see crates/math/NOTICE.
//! MathML to the element tree, for the elements and attributes pulldown-latex 0.8 writes.
//!
//! Lenient as MathML Core is: an unknown element, or a known one with the wrong children, lays
//! out as an `mrow` of its children and adds a warning. Only malformed XML and a root other than
//! `<math>` are errors.

use crate::Error;
use crate::ast::{Length, MathRoot, Node, OperatorAttrs, ScriptLevel, StyleOverrides};
use crate::mathvariant::to_math_italic;

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

/// The style attributes on an element.
fn style_overrides(node: roxmltree::Node) -> StyleOverrides {
    StyleOverrides {
        display_style: bool_attr(node, "displaystyle"),
        script_level: node.attribute("scriptlevel").and_then(parse_script_level),
        ..StyleOverrides::default()
    }
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
            let children = parse_children(node, warnings);
            let both = matches!(name, "msubsup" | "munderover");
            if children.len() != if both { 3 } else { 2 } {
                return invalid(name, children, warnings);
            }
            let mut children = children.into_iter().map(Box::new);
            let base = children.next().expect("length checked");
            let (first, second) = (children.next(), children.next());
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
        "mtable" => {
            let rows = node
                .children()
                .filter(|c| c.is_element())
                .map(|row| {
                    if row.tag_name().name() != "mtr" {
                        // Wrapped as browsers do: a stray child is a row of one cell.
                        warnings.push(format!("<{}> in <mtable>", row.tag_name().name()));
                        return vec![parse_node(row, warnings)];
                    }
                    row.children()
                        .filter(|c| c.is_element())
                        .map(|cell| {
                            if cell.tag_name().name() == "mtd" {
                                Node::Row(parse_children(cell, warnings))
                            } else {
                                warnings.push(format!("<{}> in <mtr>", cell.tag_name().name()));
                                parse_node(cell, warnings)
                            }
                        })
                        .collect()
                })
                .collect();
            Node::Table { rows }
        }
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
        "ex" => Some(Length::Ex(value)),
        "px" => Some(Length::Px(value)),
        "pt" => Some(Length::Pt(value)),
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
