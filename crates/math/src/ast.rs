// Derived from formulary 0.3.0 `src/ast.rs` (MIT OR Apache-2.0, Copyright (c) 2026 Jeffrey T.
// Peckham (Ophymx)), modified for accent; see crates/math/NOTICE.
//! The MathML element tree: what the parser builds and the layout reads.

/// A MathML length.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Length {
    /// Relative to the current font size, which shrinks in scripts.
    Em(f32),
    /// Relative to the font's x-height at the current size.
    Ex(f32),
    /// CSS pixels, taken 1:1 as layout units: the caller's size is assumed to be in px.
    Px(f32),
    /// CSS points (`1pt = 96/72 px`).
    Pt(f32),
}

/// An sRGB colour with alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// The style an element sets for its subtree, kept in a [`Node::Styled`] around it.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct StyleOverrides {
    pub display_style: Option<bool>,
    pub script_level: Option<ScriptLevel>,
    /// Ink for the subtree.
    pub color: Option<Color>,
    /// Painted behind the subtree's box.
    pub background: Option<Color>,
    /// A rule in this colour around the subtree's box.
    pub border: Option<Color>,
}

impl StyleOverrides {
    pub fn is_empty(&self) -> bool {
        *self == StyleOverrides::default()
    }
}

/// An operator's position in its row, which picks its operator-dictionary entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Form {
    Infix,
    Prefix,
    Postfix,
}

/// The `<mo>` attributes that override the operator dictionary; `None` keeps its value.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct OperatorAttrs {
    pub stretchy: Option<bool>,
    pub symmetric: Option<bool>,
    pub largeop: Option<bool>,
    pub movablelimits: Option<bool>,
    /// Clamps on the stretched size.
    pub minsize: Option<Length>,
    pub maxsize: Option<Length>,
}

/// A `scriptlevel` attribute: absolute, or relative with an explicit sign.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScriptLevel {
    Set(u8),
    Add(i8),
}

/// A parsed `<math>` element.
#[derive(Debug, Clone, PartialEq)]
pub struct MathRoot {
    /// `display="block"`: laid out for a line of its own, in display style.
    pub(crate) display: bool,
    /// The children of `<math>`, an implied `mrow`.
    pub(crate) children: Vec<Node>,
    /// What the parser did not understand and laid out as an `mrow` instead, as MathML Core's
    /// error recovery does. Empty means the markup was understood in full.
    pub warnings: Vec<String>,
}

/// One MathML element.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Node {
    /// `<mi>`, with the italic of a single letter already applied.
    Identifier(String),
    /// `<mn>`.
    Number(String),
    /// `<mo>`: spacing and stretching come from the operator dictionary, keyed by
    /// (character, form), unless an attribute overrides them.
    Operator { text: String, attrs: OperatorAttrs },
    /// `<mtext>`.
    Text(String),
    /// `<mrow>`.
    Row(Vec<Node>),
    /// `<mfrac>`. A zero `line_thickness` is a stack without a bar (`\binom`).
    Frac {
        num: Box<Node>,
        den: Box<Node>,
        line_thickness: Option<Length>,
    },
    /// `<msub>`, `<msup>` or `<msubsup>`: at least one script is present.
    Scripts {
        base: Box<Node>,
        sub: Option<Box<Node>>,
        sup: Option<Box<Node>>,
    },
    /// `<msqrt>`: the children are an implied `mrow`.
    Sqrt(Vec<Node>),
    /// `<mroot>`.
    Root { base: Box<Node>, index: Box<Node> },
    /// `<mspace>`; an unset dimension is zero.
    Space {
        width: Option<Length>,
        height: Option<Length>,
        depth: Option<Length>,
    },
    /// An element that sets a style, around that element.
    Styled {
        styles: StyleOverrides,
        children: Vec<Node>,
    },
    /// `<munder>`, `<mover>` or `<munderover>`, shaped like [`Node::Scripts`].
    UnderOver {
        base: Box<Node>,
        under: Option<Box<Node>>,
        over: Option<Box<Node>>,
        /// The overscript is an accent: tighter above the base, and at the base's size.
        accent: bool,
        /// The underscript is an accent.
        accent_under: bool,
    },
    /// `<mtable>`: rows of cells, each an implied `mrow`. A short row's missing cells are empty.
    Table { rows: Vec<Vec<Node>> },
}
