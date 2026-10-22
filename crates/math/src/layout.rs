// Derived from formulary 0.3.0 `src/layout.rs` (MIT OR Apache-2.0, Copyright (c) 2026 Jeffrey T.
// Peckham (Ophymx)), modified for accent; see crates/math/NOTICE.
//! The element tree and the font's MATH metrics to a display list.
//!
//! Every value is in the unit of the size the caller passes. `x` grows rightwards from the
//! layout's left edge, `y` downwards from the baseline at 0. `ascent` and `descent` are
//! distances above and below the baseline, so the top edge is at `y = -ascent`.

use crate::ast::{
    Align, Cell, Color, Form, Length, MathRoot, Node, ScriptLevel, StyleOverrides, TableRow,
};
use crate::font::{Constant, Corner, MathFont, Stretched};
use crate::opdict;

/// A laid-out formula: its extents and what to draw. Placed with its baseline on the
/// surrounding text's.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
    /// In painter's order: a background comes before what it is behind.
    pub items: Vec<Item>,
}

/// One thing to draw. `color: None` is the text colour around the formula.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// A glyph of the math font, its origin on the baseline at `(x, y)`, drawn at `size`, which
    /// is smaller than the layout's in scripts.
    Glyph {
        id: u16,
        x: f32,
        y: f32,
        size: f32,
        color: Option<Color>,
    },
    /// A filled rectangle, `(x, y)` its top-left corner: fraction bars, radical bars,
    /// backgrounds and borders.
    Rule {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: Option<Color>,
    },
    /// A straight stroke `thickness` wide from `(x, y)` to `(x2, y2)`: what `\not` sets across
    /// a group.
    Line {
        x: f32,
        y: f32,
        x2: f32,
        y2: f32,
        thickness: f32,
        color: Option<Color>,
    },
    /// Characters the math font lacks, drawn at `size` in the font the caller measured them
    /// with ([`MathFont::set_fallback`]), the left end of their baseline at `(x, y)`.
    Text {
        text: String,
        x: f32,
        y: f32,
        size: f32,
        color: Option<Color>,
    },
}

impl Item {
    fn translate(&mut self, dx: f32, dy: f32) {
        if let Item::Line { x2, y2, .. } = self {
            *x2 += dx;
            *y2 += dy;
        }
        let (Item::Glyph { x, y, .. }
        | Item::Rule { x, y, .. }
        | Item::Line { x, y, .. }
        | Item::Text { x, y, .. }) = self;
        *x += dx;
        *y += dy;
    }
}

/// The size factor of a script level, from the font's scale-down percentages.
fn script_factor(font: &MathFont, script_level: u8) -> f32 {
    let percent = |c, fallback| match font.percent(c) {
        p if p > 0.0 => p,
        _ => fallback,
    };
    match script_level {
        0 => 1.0,
        1 => percent(Constant::ScriptPercentScaleDown, 0.71),
        _ => percent(Constant::ScriptScriptPercentScaleDown, 0.5041),
    }
}

/// Lays out a parsed formula in `font` at `size`.
pub fn layout(root: &MathRoot, font: &MathFont, size: f32) -> Layout {
    // A block formula starts in display style, an inline one in text style.
    let ctx = Ctx::derive(font, size, 0, root.display, false, None);
    let b = layout_row(&ctx, &root.children);
    Layout {
        // Negative space (`\!`) can pull a row narrower than nothing.
        width: b.width.max(0.0),
        ascent: b.ascent,
        descent: b.descent,
        items: b.items,
    }
}

/// The state passed down the tree: MathML Core's math-style, math-depth and cramped, and the
/// scale they come to.
struct Ctx<'a> {
    font: &'a MathFont,
    /// The caller's size, which script sizes derive from.
    base_size: f32,
    /// math-depth: 0 at the top, one more in scripts and text-style fraction parts.
    script_level: u8,
    /// Display style: big operators, wider fraction gaps. Off in fraction parts and scripts.
    display_style: bool,
    /// Cramped styles (under bars, in subscripts and denominators) raise superscripts less.
    cramped: bool,
    /// The colour set above; `None` is the caller's text colour.
    color: Option<Color>,
    /// The size at this script level, stamped on glyphs.
    size: f32,
    /// Design units to output units at this script level.
    scale: f32,
}

impl<'a> Ctx<'a> {
    fn derive(
        font: &'a MathFont,
        base_size: f32,
        script_level: u8,
        display_style: bool,
        cramped: bool,
        color: Option<Color>,
    ) -> Self {
        let size = base_size * script_factor(font, script_level);
        Ctx {
            font,
            base_size,
            script_level,
            display_style,
            cramped,
            color,
            size,
            scale: size / font.units_per_em(),
        }
    }

    fn child(&self, script_level: u8, display_style: bool, cramped: bool) -> Self {
        Ctx::derive(
            self.font,
            self.base_size,
            script_level,
            display_style,
            self.cramped || cramped,
            self.color,
        )
    }

    /// A fraction part: display style ends; the level rises only from text style.
    fn frac_child(&self, cramped: bool) -> Self {
        let level = if self.display_style {
            self.script_level
        } else {
            self.script_level.saturating_add(1)
        };
        self.child(level, false, cramped)
    }

    /// Under a radical: the same size and style, cramped.
    fn cramped_child(&self) -> Self {
        self.child(self.script_level, self.display_style, true)
    }

    /// An `<mroot>` degree: two levels up, as TeX's scriptscript style.
    fn root_degree_child(&self) -> Self {
        self.child(self.script_level.saturating_add(2), false, false)
    }

    /// An accent: no level rise, so the mark stays as wide as its base.
    fn accent_child(&self, cramped: bool) -> Self {
        self.child(self.script_level, false, cramped)
    }

    /// A subscript or superscript: one level up, out of display style.
    fn script_child(&self, cramped: bool) -> Self {
        self.child(self.script_level.saturating_add(1), false, cramped)
    }

    /// Under an element that sets a style.
    fn styled_child(&self, styles: &StyleOverrides) -> Self {
        let script_level = match styles.script_level {
            None => self.script_level,
            Some(ScriptLevel::Set(n)) => n,
            Some(ScriptLevel::Add(d)) => {
                (i16::from(self.script_level) + i16::from(d)).clamp(0, 255) as u8
            }
        };
        Ctx::derive(
            self.font,
            self.base_size * styles.size.unwrap_or(1.0),
            script_level,
            styles.display_style.unwrap_or(self.display_style),
            self.cramped,
            styles.color.or(self.color),
        )
    }

    /// A MATH constant in output units at this level.
    fn constant(&self, c: Constant) -> f32 {
        self.font.constant(c) * self.scale
    }

    /// A glyph item at `(x, y)` in this context's size and colour.
    fn glyph_item(&self, id: u16, x: f32, y: f32) -> Item {
        Item::Glyph {
            id,
            x,
            y,
            size: self.size,
            color: self.color,
        }
    }

    /// The font's `ssty` alternate in script styles.
    fn script_glyph(&self, glyph: u16) -> u16 {
        if self.script_level == 0 {
            return glyph;
        }
        self.font
            .script_alternate(glyph, self.script_level.min(2))
            .unwrap_or(glyph)
    }

    /// A MathML length in output units.
    fn resolve(&self, len: Length) -> f32 {
        match len {
            Length::Em(v) => v * self.size,
            Length::Ex(v) => v * self.font.x_height() * self.scale,
            Length::Px(v) => v,
        }
    }
}

/// A box on the way up: like [`Layout`], with items relative to its own baseline and left edge.
#[derive(Default)]
struct MathBox {
    width: f32,
    ascent: f32,
    descent: f32,
    /// The italic correction of the box's last glyph (glyph runs and large operators, 0 for
    /// anything built), which script attachment takes up.
    italic_correction: f32,
    /// The glyph, when the box is exactly one: where the font's MathKern and top-accent data
    /// apply.
    lone_glyph: Option<u16>,
    items: Vec<Item>,
}

fn layout_node(ctx: &Ctx, node: &Node) -> MathBox {
    match node {
        // An <mo> met outside a row (as a script base, say) takes no form-dependent spacing:
        // the row around it handles that.
        Node::Identifier(text)
        | Node::Operator { text, .. }
        | Node::Number(text)
        | Node::Text(text) => layout_text_run(ctx, text),
        // A one-child mrow is transparent: its child's operator spacing and embellishment
        // belong to the row around, so no inner row spaces it a second time.
        Node::Row(children) if children.len() == 1 => layout_node(ctx, &children[0]),
        Node::Row(children) => layout_row(ctx, children),
        Node::Frac {
            num,
            den,
            line_thickness,
        } => layout_frac(ctx, num, den, *line_thickness),
        Node::Scripts { base, sub, sup } => {
            layout_scripts(ctx, base, sub.as_deref(), sup.as_deref())
        }
        Node::UnderOver {
            base,
            under,
            over,
            accent,
            accent_under,
        } => layout_underover(
            ctx,
            base,
            under.as_deref(),
            over.as_deref(),
            *accent,
            *accent_under,
        ),
        Node::Bar { base, under } => layout_bar(ctx, base, *under),
        Node::Table(rows) => layout_table(ctx, rows),
        Node::Sqrt(children) => {
            let content = layout_row(&ctx.cramped_child(), children);
            layout_radical(ctx, content, None)
        }
        Node::Root { base, index } => {
            let content = layout_node(&ctx.cramped_child(), base);
            let degree = layout_node(&ctx.root_degree_child(), index);
            layout_radical(ctx, content, Some(degree))
        }
        Node::Space {
            width,
            height,
            depth,
        } => {
            let length = |l: &Option<Length>| l.map_or(0.0, |l| ctx.resolve(l)).max(0.0);
            MathBox {
                // A negative width (`\!`, pulldown-latex 454-456) pulls what follows back.
                width: width.map_or(0.0, |l| ctx.resolve(l)),
                ascent: length(height),
                descent: length(depth),
                ..MathBox::default()
            }
        }
        Node::Styled { styles, children } => layout_styled(ctx, styles, children),
    }
}

/// Lays the children out in the scope's style, then paints its background behind them and
/// its border around them.
fn layout_styled(ctx: &Ctx, styles: &StyleOverrides, children: &[Node]) -> MathBox {
    let inner = ctx.styled_child(styles);
    // Around one element the scope is transparent, as a one-child mrow is: the row around
    // owns the child's operator spacing.
    let b = match children {
        [only] => layout_node(&inner, only),
        _ => layout_row(&inner, children),
    };
    decorate(&inner, styles, b)
}

/// The scope's border around its box, 0.06 em as pulldown-latex writes it (113) and the box
/// grown by it, its background behind the lot, and its stroke over it.
fn decorate(ctx: &Ctx, styles: &StyleOverrides, mut b: MathBox) -> MathBox {
    if let Some(color) = styles.border {
        let t = 0.06 * ctx.size;
        for item in &mut b.items {
            item.translate(t, 0.0);
        }
        b.width += 2.0 * t;
        b.ascent += t;
        b.descent += t;
        b.italic_correction = 0.0;
        b.lone_glyph = None;
        let (w, h, top) = (b.width, b.ascent + b.descent, -b.ascent);
        let edge = |x, y, w, h| Item::Rule {
            x,
            y,
            w,
            h,
            color: Some(color),
        };
        b.items.extend([
            edge(0.0, top, w, t),
            edge(0.0, top + h - t, w, t),
            edge(0.0, top, t, h),
            edge(w - t, top, t, h),
        ]);
    }
    if let Some(color) = styles.background {
        let (x, y, w, h, color) = (0.0, -b.ascent, b.width, b.ascent + b.descent, Some(color));
        b.items.insert(0, Item::Rule { x, y, w, h, color });
    }
    // From the bottom-left corner to the top-right, as `styles.css` strikes `mop-negated`
    // through, as thick as pulldown-latex's rules.
    if styles.strike {
        b.items.push(Item::Line {
            x: 0.0,
            y: b.descent,
            x2: b.width,
            y2: -b.ascent,
            thickness: 0.06 * ctx.size,
            color: ctx.color,
        });
    }
    b
}

/// `\overline` and `\underline` as TeX draws them, by the MATH bar constants: a rule as wide
/// as the base, a gap from it, and space beyond. An overlined base is cramped (TeX's rule 9).
fn layout_bar(ctx: &Ctx, base: &Node, under: bool) -> MathBox {
    let mut b = if under {
        layout_node(ctx, base)
    } else {
        layout_node(&ctx.cramped_child(), base)
    };
    let (gap, t, extra) = if under {
        (
            ctx.constant(Constant::UnderbarVerticalGap),
            ctx.constant(Constant::UnderbarRuleThickness),
            ctx.constant(Constant::UnderbarExtraDescender),
        )
    } else {
        (
            ctx.constant(Constant::OverbarVerticalGap),
            ctx.constant(Constant::OverbarRuleThickness),
            ctx.constant(Constant::OverbarExtraAscender),
        )
    };
    let y = if under {
        b.descent + gap
    } else {
        -(b.ascent + gap + t)
    };
    b.items.push(Item::Rule {
        x: 0.0,
        y,
        w: b.width,
        h: t,
        color: ctx.color,
    });
    if under {
        b.descent += gap + t + extra;
    } else {
        b.ascent += gap + t + extra;
    }
    b.italic_correction = 0.0;
    b.lone_glyph = None;
    b
}

/// `<mtable>`: cells on their row's baseline, columns as wide as their widest cell, and the
/// table centred on the math axis. Cells are in text style and padded 0.5 ex above and below,
/// as MathML Core has them; the space beside them, their alignment and the rules are what
/// pulldown-latex's classes ask (`parse_table`).
fn layout_table(ctx: &Ctx, rows: &[TableRow]) -> MathBox {
    let cell_ctx = ctx.styled_child(&StyleOverrides {
        display_style: Some(false),
        ..StyleOverrides::default()
    });
    let em = cell_ctx.size;
    let rule = 0.06 * em;
    let vpad = 0.5 * ctx.font.x_height() * ctx.scale;

    // Each cell's box, and what its padding and rules take on either side.
    let cells: Vec<Vec<(MathBox, f32, f32)>> = rows
        .iter()
        .map(|row| {
            let side = |pad: f32, ruled: bool| pad * em + if ruled { rule } else { 0.0 };
            let lay = |c: &Cell| {
                let b = layout_node(&cell_ctx, &c.content);
                (b, side(c.pad.0, c.rules.0), side(c.pad.1, c.rules.1))
            };
            row.cells.iter().map(lay).collect()
        })
        .collect();
    let mut col_widths = vec![0.0_f32; cells.iter().map(Vec::len).max().unwrap_or(0)];
    for row in &cells {
        for (w, (b, left, right)) in col_widths.iter_mut().zip(row) {
            *w = w.max(left + b.width + right);
        }
    }
    // A row's extents, padded; a row without cells (what a trailing `\\` leaves) takes none.
    let extents: Vec<(f32, f32)> = rows
        .iter()
        .zip(&cells)
        .map(|(row, cells)| {
            if cells.is_empty() {
                return (0.0, 0.0);
            }
            // The empty row between two `\hline`s is 0.1 em tall (`styles.css`).
            if row.spacer && row.rule_above {
                return (0.1 * em, 0.0);
            }
            let ascent = vpad + cells.iter().map(|c| c.0.ascent).fold(0.0, f32::max);
            let descent = vpad + cells.iter().map(|c| c.0.descent).fold(0.0, f32::max);
            let min = row.min_height.map_or(0.0, |l| cell_ctx.resolve(l));
            (ascent, descent.max(min - ascent))
        })
        .collect();

    let total_width: f32 = col_widths.iter().sum();
    let total_height: f32 = extents.iter().map(|(a, d)| a + d).sum();
    // Centred on the math axis; a table shorter than twice the axis height sits on the
    // baseline rather than dipping below it.
    let axis = ctx.constant(Constant::AxisHeight);
    let descent = (total_height / 2.0 - axis).max(0.0);
    let mut out = MathBox {
        width: total_width,
        ascent: total_height - descent,
        descent,
        ..MathBox::default()
    };

    let line = |x, y, w, h| Item::Rule {
        x,
        y,
        w,
        h,
        color: ctx.color,
    };
    let mut top = -out.ascent;
    for ((row, cells), (ascent, descent)) in rows.iter().zip(cells).zip(extents) {
        let (baseline, bottom) = (top + ascent, top + ascent + descent);
        if row.rule_above {
            out.items.push(line(0.0, top, total_width, rule));
        }
        let mut x = 0.0;
        for ((cell, (b, left, right)), w) in row.cells.iter().zip(cells).zip(&col_widths) {
            let slack = w - left - b.width - right;
            let dx = x
                + left
                + match cell.align {
                    Align::Left => 0.0,
                    Align::Center => slack / 2.0,
                    Align::Right => slack,
                };
            for mut item in b.items {
                item.translate(dx, baseline);
                out.items.push(item);
            }
            if cell.rules.0 {
                out.items.push(line(x, top, rule, bottom - top));
            }
            if cell.rules.1 {
                out.items.push(line(x + w - rule, top, rule, bottom - top));
            }
            x += w;
        }
        top = bottom;
    }
    out
}

const RADICAL_CHAR: char = '\u{221A}';

/// `<msqrt>` and `<mroot>` by the MATH radical constants: the radical glyph stretched (a
/// variant or an assembly) over the radicand's height plus the least gap and the rule; the bar
/// runs on from its top across the radicand, with RadicalExtraAscender of space above.
fn layout_radical(ctx: &Ctx, content: MathBox, degree: Option<MathBox>) -> MathBox {
    let gap_min = if ctx.display_style {
        ctx.constant(Constant::RadicalDisplayStyleVerticalGap)
    } else {
        ctx.constant(Constant::RadicalVerticalGap)
    };
    let thickness = ctx.constant(Constant::RadicalRuleThickness);

    let target = content.ascent + content.descent + gap_min + thickness;
    let stretched = ctx
        .font
        .glyph_index(RADICAL_CHAR)
        .map(|base| ctx.font.stretch_vertical(base, target / ctx.scale));
    let glyph_height = stretched
        .as_ref()
        .map_or(0.0, |s| emit_stretched(ctx, s, 0.0, 0.0, &mut Vec::new()).0);

    // A glyph taller than needed shares the excess: half widens the gap, half hangs below
    // the radicand (TeX's rule 11).
    let gap = gap_min + (glyph_height - target).max(0.0) / 2.0;
    // The bar's top edge, which the radical glyph's ink top meets.
    let bar_top = -(content.ascent + gap + thickness);

    let mut out = MathBox {
        ascent: content.ascent + gap + thickness + ctx.constant(Constant::RadicalExtraAscender),
        descent: content.descent.max(bar_top + glyph_height),
        ..MathBox::default()
    };

    // [kern degree kern][surd][radicand]
    let mut x = 0.0;
    if let Some(deg) = degree {
        x += ctx.constant(Constant::RadicalKernBeforeDegree);
        // The degree's bottom sits this share of the glyph's height above the glyph's bottom.
        let raise = ctx.font.percent(Constant::RadicalDegreeBottomRaisePercent);
        let deg_baseline = bar_top + glyph_height - raise * glyph_height - deg.descent;
        out.ascent = out.ascent.max(deg.ascent - deg_baseline);
        out.descent = out.descent.max(deg_baseline + deg.descent);
        for mut item in deg.items {
            item.translate(x, deg_baseline);
            out.items.push(item);
        }
        x += deg.width + ctx.constant(Constant::RadicalKernAfterDegree);
        x = x.max(0.0); // a large negative kern must not push the glyph out
    }
    if let Some(s) = &stretched {
        x += emit_stretched(ctx, s, x, bar_top, &mut out.items).1;
    }
    out.items.push(Item::Rule {
        x,
        y: bar_top,
        w: content.width,
        h: thickness,
        color: ctx.color,
    });
    for mut item in content.items {
        item.translate(x, 0.0);
        out.items.push(item);
    }
    out.width = x + content.width;
    out
}

/// `<munder>`, `<mover>` and `<munderover>`.
///
/// Movable limits are scripts outside display style. An accent keeps full size and sits on
/// the base at AccentBaseHeight; limits on a large operator take the Upper/LowerLimit
/// constants, anything else the over/underbar gaps. A lone stretchy horizontal operator in any
/// slot stretches to the widest slot's width.
fn layout_underover(
    ctx: &Ctx,
    base: &Node,
    under: Option<&Node>,
    over: Option<&Node>,
    accent: bool,
    accent_under: bool,
) -> MathBox {
    let base_flags = core_operator(base)
        .map(|(text, _)| dictionary_entry(text, Form::Infix).2)
        .unwrap_or(0);
    if !ctx.display_style {
        let movable = core_operator(base).is_some_and(|(_, attrs)| {
            attrs
                .movablelimits
                .unwrap_or(base_flags & opdict::MOVABLE_LIMITS != 0)
        });
        if movable {
            return layout_scripts(ctx, base, under, over);
        }
    }

    let base_box = layout_operator_base(ctx, base);
    let over_ctx = if accent {
        ctx.accent_child(false)
    } else {
        ctx.script_child(false)
    };
    let under_ctx = if accent_under {
        ctx.accent_child(true)
    } else {
        ctx.script_child(true)
    };
    let over_box = over.map(|n| layout_node(&over_ctx, n));
    let under_box = under.map(|n| layout_node(&under_ctx, n));

    let width = base_box
        .width
        .max(over_box.as_ref().map_or(0.0, |b| b.width))
        .max(under_box.as_ref().map_or(0.0, |b| b.width));
    // An accent stretches across, whatever the dictionary says of its character.
    let stretch_h = |ctx: &Ctx, node: &Node, natural: MathBox, accent: bool| -> MathBox {
        if natural.width >= width - 1e-3 {
            return natural;
        }
        let Node::Operator { text, attrs } = node else {
            return natural;
        };
        let flags = dictionary_entry(text, Form::Infix).2;
        let stretchy = attrs.stretchy.unwrap_or(flags & opdict::STRETCHY != 0)
            && (flags & opdict::HORIZONTAL != 0 || accent);
        match single_char(text).and_then(|c| ctx.font.glyph_index(c)) {
            Some(g) if stretchy => {
                layout_stretched_horizontal(ctx, &ctx.font.stretch_horizontal(g, width / ctx.scale))
            }
            _ => natural,
        }
    };
    let base_box = stretch_h(ctx, base, base_box, false);
    let over_box = over
        .zip(over_box)
        .map(|(n, b)| stretch_h(&over_ctx, n, b, accent));
    let under_box = under
        .zip(under_box)
        .map(|(n, b)| stretch_h(&under_ctx, n, b, accent_under));

    // Operators with limits take the limit constants, other bases the bar constants.
    let limits_base = base_flags & (opdict::LARGEOP | opdict::MOVABLE_LIMITS) != 0;

    let mut out = MathBox {
        width,
        ascent: base_box.ascent,
        descent: base_box.descent,
        ..MathBox::default()
    };
    let base_dx = (width - base_box.width) / 2.0;
    // Where an accent attaches: the font's point when the box is one glyph, else its middle.
    let attach = |b: &MathBox| {
        b.lone_glyph.map_or(b.width / 2.0, |g| {
            ctx.font.top_accent_attachment(g) * ctx.scale
        })
    };
    let base_attach = base_dx + attach(&base_box);
    for mut item in base_box.items {
        item.translate(base_dx, 0.0);
        out.items.push(item);
    }

    if let Some(ob) = over_box {
        let (shift_up, extra_ascender) = if accent {
            // An accent glyph is drawn for a base of AccentBaseHeight: it rises only by what
            // the base has above that.
            (
                (base_box.ascent - ctx.constant(Constant::AccentBaseHeight)).max(0.0),
                0.0,
            )
        } else if limits_base {
            let rise = ctx.constant(Constant::UpperLimitBaselineRiseMin);
            let gap = ctx.constant(Constant::UpperLimitGapMin);
            (base_box.ascent + rise.max(gap + ob.descent), 0.0)
        } else {
            (
                base_box.ascent + ctx.constant(Constant::OverbarVerticalGap) + ob.descent,
                ctx.constant(Constant::OverbarExtraAscender),
            )
        };
        out.ascent = out.ascent.max(shift_up + ob.ascent + extra_ascender);
        // An accent lines up the attachment points; other overscripts are centred.
        let dx = if accent {
            base_attach - attach(&ob)
        } else {
            (width - ob.width) / 2.0
        };
        for mut item in ob.items {
            item.translate(dx, -shift_up);
            out.items.push(item);
        }
    }

    if let Some(ub) = under_box {
        let (shift_down, extra_descender) = if accent_under {
            ((base_box.descent + ub.ascent).max(0.0), 0.0)
        } else if limits_base {
            let drop = ctx.constant(Constant::LowerLimitBaselineDropMin);
            let gap = ctx.constant(Constant::LowerLimitGapMin);
            (base_box.descent + drop.max(gap + ub.ascent), 0.0)
        } else {
            (
                base_box.descent + ctx.constant(Constant::UnderbarVerticalGap) + ub.ascent,
                ctx.constant(Constant::UnderbarExtraDescender),
            )
        };
        out.descent = out.descent.max(shift_down + ub.descent + extra_descender);
        let dx = (width - ub.width) / 2.0;
        for mut item in ub.items {
            item.translate(dx, shift_down);
            out.items.push(item);
        }
    }
    out
}

/// A fraction without a bar (`linethickness="0"`), by the MATH stack constants: the preferred
/// shifts, then both parts pushed apart evenly until StackGapMin holds.
fn layout_stack(ctx: &Ctx, num_box: MathBox, den_box: MathBox) -> MathBox {
    let (mut num_shift, mut den_shift, gap_min) = if ctx.display_style {
        (
            ctx.constant(Constant::StackTopDisplayStyleShiftUp),
            ctx.constant(Constant::StackBottomDisplayStyleShiftDown),
            ctx.constant(Constant::StackDisplayStyleGapMin),
        )
    } else {
        (
            ctx.constant(Constant::StackTopShiftUp),
            ctx.constant(Constant::StackBottomShiftDown),
            ctx.constant(Constant::StackGapMin),
        )
    };
    let gap = (num_shift - num_box.descent) + (den_shift - den_box.ascent);
    if gap < gap_min {
        let bump = (gap_min - gap) / 2.0;
        num_shift += bump;
        den_shift += bump;
    }
    stack(num_box, num_shift, den_box, den_shift)
}

/// Numerator and denominator centred over each other at their shifts.
fn stack(num_box: MathBox, num_shift: f32, den_box: MathBox, den_shift: f32) -> MathBox {
    let width = num_box.width.max(den_box.width);
    let mut out = MathBox {
        width,
        ascent: num_shift + num_box.ascent,
        descent: den_shift + den_box.descent,
        items: Vec::with_capacity(num_box.items.len() + den_box.items.len() + 1),
        ..MathBox::default()
    };
    for (bx, dy) in [(num_box, -num_shift), (den_box, den_shift)] {
        let dx = (width - bx.width) / 2.0;
        for mut item in bx.items {
            item.translate(dx, dy);
            out.items.push(item);
        }
    }
    out
}

/// `<msub>`, `<msup>` and `<msubsup>` by the MATH script constants (TeX's Appendix G rules
/// 18a–f in font terms).
fn layout_scripts(ctx: &Ctx, base: &Node, sub: Option<&Node>, sup: Option<&Node>) -> MathBox {
    let base_box = layout_operator_base(ctx, base);
    layout_scripts_on(ctx, base_box, sub, sup)
}

/// The MathKern cut-in of one script against its base: the base's corner at the script's near
/// edge plus the script's corner at the base's near edge, each height from that glyph's
/// baseline. Only a lone glyph has kern data; anything else adds nothing.
fn script_kern(
    ctx: &Ctx,
    (base, base_corner, base_height): (&MathBox, Corner, f32),
    (script, script_corner, script_height): (&MathBox, Corner, f32),
) -> f32 {
    let mut kern = 0.0;
    if let Some(g) = base.lone_glyph {
        kern += ctx.font.math_kern(g, base_corner, base_height / ctx.scale) * ctx.scale;
    }
    if let Some(g) = script.lone_glyph {
        // The script glyph was laid out at the script's scale.
        let script_scale = match script.items.first() {
            Some(Item::Glyph { size, .. }) => size / ctx.font.units_per_em(),
            _ => ctx.scale,
        };
        kern += ctx
            .font
            .math_kern(g, script_corner, script_height / script_scale)
            * script_scale;
    }
    kern
}

/// How far a superscript rises and a subscript drops (TeX's u and v).
fn script_shifts(
    ctx: &Ctx,
    base: &MathBox,
    sub: Option<&MathBox>,
    sup: Option<&MathBox>,
) -> (f32, f32) {
    let sup_shift = sup.map_or(0.0, |s| {
        let preferred = if ctx.cramped {
            ctx.constant(Constant::SuperscriptShiftUpCramped)
        } else {
            ctx.constant(Constant::SuperscriptShiftUp)
        };
        preferred
            // Not too far below the base's top,
            .max(base.ascent - ctx.constant(Constant::SuperscriptBaselineDropMax))
            // and its ink above SuperscriptBottomMin.
            .max(ctx.constant(Constant::SuperscriptBottomMin) + s.descent)
    });
    let sub_shift = sub.map_or(0.0, |s| {
        ctx.constant(Constant::SubscriptShiftDown)
            // At least this far below the base's bottom,
            .max(base.descent + ctx.constant(Constant::SubscriptBaselineDropMin))
            // and its ink below SubscriptTopMax.
            .max(s.ascent - ctx.constant(Constant::SubscriptTopMax))
    });
    let (Some(sub), Some(sup)) = (sub, sup) else {
        return (sup_shift, sub_shift);
    };
    // With both, keep them apart: raise the superscript first (up to
    // SuperscriptBottomMaxWithSubscript), then push the subscript down.
    let (mut sup_shift, mut sub_shift) = (sup_shift, sub_shift);
    let gap = (sup_shift - sup.descent) + (sub_shift - sub.ascent);
    let mut deficit = ctx.constant(Constant::SubSuperscriptGapMin) - gap;
    if deficit > 0.0 {
        let headroom =
            ctx.constant(Constant::SuperscriptBottomMaxWithSubscript) - (sup_shift - sup.descent);
        if headroom > 0.0 {
            let up = deficit.min(headroom);
            sup_shift += up;
            deficit -= up;
        }
        sub_shift += deficit.max(0.0);
    }
    (sup_shift, sub_shift)
}

/// Scripts on a base already laid out, which an embellished stretchy operator stretches first.
fn layout_scripts_on(
    ctx: &Ctx,
    base_box: MathBox,
    sub: Option<&Node>,
    sup: Option<&Node>,
) -> MathBox {
    let sub_box = sub.map(|n| layout_node(&ctx.script_child(true), n));
    let sup_box = sup.map(|n| layout_node(&ctx.script_child(false), n));
    let (sup_shift, sub_shift) = script_shifts(ctx, &base_box, sub_box.as_ref(), sup_box.as_ref());

    // A superscript attaches at the full advance, a subscript tucks left by the base's italic
    // correction (the ∫ lower-limit tuck); then the MathKern staircases cut both into the
    // base's corners at the heights they sit at.
    let sup_kern = sup_box.as_ref().map_or(0.0, |s| {
        script_kern(
            ctx,
            (&base_box, Corner::TopRight, sup_shift - s.descent),
            (s, Corner::BottomLeft, base_box.ascent - sup_shift),
        )
    });
    let sub_kern = sub_box.as_ref().map_or(0.0, |s| {
        script_kern(
            ctx,
            (&base_box, Corner::BottomRight, s.ascent - sub_shift),
            (s, Corner::TopLeft, sub_shift - base_box.descent),
        )
    });
    let sup_x = (base_box.width + sup_kern).max(0.0);
    let sub_x = (base_box.width - base_box.italic_correction + sub_kern).max(0.0);
    let end = sub_box
        .as_ref()
        .map_or(0.0, |b| sub_x + b.width)
        .max(sup_box.as_ref().map_or(0.0, |b| sup_x + b.width))
        .max(base_box.width);

    let mut out = MathBox {
        width: end + ctx.constant(Constant::SpaceAfterScript),
        ascent: base_box.ascent,
        descent: base_box.descent,
        items: base_box.items,
        ..MathBox::default()
    };
    if let Some(s) = sup_box {
        out.ascent = out.ascent.max(sup_shift + s.ascent);
        out.descent = out.descent.max(s.descent - sup_shift);
        for mut item in s.items {
            item.translate(sup_x, -sup_shift);
            out.items.push(item);
        }
    }
    if let Some(s) = sub_box {
        out.ascent = out.ascent.max(s.ascent - sub_shift);
        out.descent = out.descent.max(sub_shift + s.descent);
        for mut item in s.items {
            item.translate(sub_x, sub_shift);
            out.items.push(item);
        }
    }
    out
}

/// `<mfrac>` by MathML Core and the MATH fraction constants: the numerator up and the
/// denominator down by at least the preferred shifts, further apart if the least gaps to the
/// rule ask it, and the rule centred on the math axis. A zero `linethickness` is a stack.
fn layout_frac(ctx: &Ctx, num: &Node, den: &Node, line_thickness: Option<Length>) -> MathBox {
    let num_box = layout_node(&ctx.frac_child(false), num);
    let den_box = layout_node(&ctx.frac_child(true), den);

    let axis = ctx.constant(Constant::AxisHeight);
    let thickness = line_thickness.map_or(ctx.constant(Constant::FractionRuleThickness), |l| {
        ctx.resolve(l).max(0.0)
    });
    if thickness == 0.0 {
        return layout_stack(ctx, num_box, den_box);
    }
    let (shift_up, shift_down, gap_above, gap_below) = if ctx.display_style {
        (
            ctx.constant(Constant::FractionNumeratorDisplayStyleShiftUp),
            ctx.constant(Constant::FractionDenominatorDisplayStyleShiftDown),
            ctx.constant(Constant::FractionNumDisplayStyleGapMin),
            ctx.constant(Constant::FractionDenomDisplayStyleGapMin),
        )
    } else {
        (
            ctx.constant(Constant::FractionNumeratorShiftUp),
            ctx.constant(Constant::FractionDenominatorShiftDown),
            ctx.constant(Constant::FractionNumeratorGapMin),
            ctx.constant(Constant::FractionDenominatorGapMin),
        )
    };
    let num_shift = shift_up.max(axis + thickness / 2.0 + gap_above + num_box.descent);
    let den_shift = shift_down.max(gap_below + thickness / 2.0 - axis + den_box.ascent);
    let mut out = stack(num_box, num_shift, den_box, den_shift);
    out.items.push(Item::Rule {
        x: 0.0,
        y: -(axis + thickness / 2.0),
        w: out.width,
        h: thickness,
        color: ctx.color,
    });
    out
}

/// Children side by side on one baseline, `<mo>` children spaced by the operator dictionary
/// and the vertically stretchy ones stretched to the row.
fn layout_row(ctx: &Ctx, children: &[Node]) -> MathBox {
    // An operator's form comes from its place among the siblings that are not space-like.
    let significant: Vec<usize> = children
        .iter()
        .enumerate()
        .filter(|(_, c)| !is_space_like(c))
        .map(|(i, _)| i)
        .collect();

    // Pass 1: everything but the vertically stretchy (perhaps embellished) operators, which
    // wait for the row's extent. Spacing and stretchiness come from the embellished
    // operator's core `<mo>`, so `<msup><mo>)</mo>…</msup>` spaces and stretches like the
    // fence it wraps.
    enum Slot<'n> {
        Fixed(MathBox),
        Stretchy { node: &'n Node, symmetric: bool },
    }
    let mut slots: Vec<(f32, f32, Slot)> = Vec::with_capacity(children.len());
    let (mut max_ascent, mut max_descent) = (0.0_f32, 0.0_f32);
    for (i, child) in children.iter().enumerate() {
        let slot = if let Some((text, attrs)) = core_operator(child) {
            let (lspace, rspace, flags) = dictionary_entry(text, infer_form(i, &significant));
            let em = ctx.size / 18.0;
            let (lspace, rspace) = (f32::from(lspace) * em, f32::from(rspace) * em);
            let stretchy = attrs.stretchy.unwrap_or(flags & opdict::STRETCHY != 0)
                && flags & opdict::HORIZONTAL == 0
                && single_char(text)
                    .and_then(|c| ctx.font.glyph_index(c))
                    .is_some();
            let slot = if stretchy {
                let symmetric = attrs.symmetric.unwrap_or(flags & opdict::SYMMETRIC != 0);
                Slot::Stretchy {
                    node: child,
                    symmetric,
                }
            } else if let Node::Operator { text, attrs } = child {
                // A bare <mo> becomes a large operator here; an embellished one in
                // layout_operator_base.
                match single_char(text) {
                    Some(c)
                        if ctx.display_style
                            && attrs.largeop.unwrap_or(flags & opdict::LARGEOP != 0) =>
                    {
                        Slot::Fixed(layout_large_operator(ctx, c))
                    }
                    _ => Slot::Fixed(layout_text_run(ctx, text)),
                }
            } else {
                // pulldown-latex's `\displaystyle` puts a style on the `<mo>` itself.
                Slot::Fixed(layout_operator_base(ctx, child))
            };
            (lspace, rspace, slot)
        } else {
            (0.0, 0.0, Slot::Fixed(layout_node(ctx, child)))
        };
        if let (_, _, Slot::Fixed(b)) = &slot {
            max_ascent = max_ascent.max(b.ascent);
            max_descent = max_descent.max(b.descent);
        }
        slots.push(slot);
    }

    // Pass 2: the deferred operators stretched to the row's extent, then everything placed.
    let mut out = MathBox::default();
    for (lspace, rspace, slot) in slots {
        let mut b = match slot {
            Slot::Fixed(b) => b,
            Slot::Stretchy { node, symmetric } => {
                layout_embellished_stretchy(ctx, node, symmetric, max_ascent, max_descent)
            }
        };
        for item in &mut b.items {
            item.translate(out.width + lspace, 0.0);
        }
        out.items.append(&mut b.items);
        out.ascent = out.ascent.max(b.ascent);
        out.descent = out.descent.max(b.descent);
        out.italic_correction = b.italic_correction;
        out.width += lspace + b.width + rspace;
    }
    out
}

/// The core of an embellished operator stretched to the row's extent, then given back its
/// scripts and style scopes. Under/over embellishments lay out unstretched.
fn layout_embellished_stretchy(
    ctx: &Ctx,
    node: &Node,
    symmetric: bool,
    max_ascent: f32,
    max_descent: f32,
) -> MathBox {
    match node {
        Node::Styled { styles, children } if children.len() == 1 => {
            let inner = ctx.styled_child(styles);
            let core = layout_embellished_stretchy(
                &inner,
                &children[0],
                symmetric,
                max_ascent,
                max_descent,
            );
            decorate(&inner, styles, core)
        }
        Node::Operator { text, attrs } => match single_char(text) {
            Some(c) => layout_stretchy_operator(
                ctx,
                c,
                symmetric,
                (attrs.minsize, attrs.maxsize),
                max_ascent,
                max_descent,
            ),
            None => layout_node(ctx, node),
        },
        Node::Scripts { base, sub, sup } => {
            let base_box =
                layout_embellished_stretchy(ctx, base, symmetric, max_ascent, max_descent);
            layout_scripts_on(ctx, base_box, sub.as_deref(), sup.as_deref())
        }
        _ => layout_node(ctx, node),
    }
}

/// A box of one glyph at its natural size, with a one-character run's metrics.
fn natural_glyph_box(ctx: &Ctx, glyph: u16) -> MathBox {
    let ink = ctx.font.ink_box(glyph);
    MathBox {
        width: ctx.font.advance(glyph) * ctx.scale,
        ascent: ink.map_or(0.0, |i| i.y_max * ctx.scale),
        descent: ink.map_or(0.0, |i| -i.y_min * ctx.scale),
        italic_correction: ctx.font.italic_correction(glyph) * ctx.scale,
        lone_glyph: Some(glyph),
        items: vec![ctx.glyph_item(glyph, 0.0, 0.0)],
    }
}

fn single_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// Emits a [`Stretched`] glyph with its ink top at `top`, and gives back its ink height and
/// advance, all in output units.
fn emit_stretched(
    ctx: &Ctx,
    stretched: &Stretched,
    x: f32,
    top: f32,
    items: &mut Vec<Item>,
) -> (f32, f32) {
    match stretched {
        Stretched::Glyph(g) => {
            let advance = ctx.font.advance(*g) * ctx.scale;
            let Some(ink) = ctx.font.ink_box(*g) else {
                return (0.0, advance);
            };
            items.push(ctx.glyph_item(*g, x, top + ink.y_max * ctx.scale));
            ((ink.y_max - ink.y_min) * ctx.scale, advance)
        }
        Stretched::Assembly { parts, extent } => {
            let height = extent * ctx.scale;
            let bottom = top + height;
            let mut advance = 0.0_f32;
            for &(g, offset) in parts {
                advance = advance.max(ctx.font.advance(g) * ctx.scale);
                let Some(ink) = ctx.font.ink_box(g) else {
                    continue;
                };
                // The part's ink bottom sits `offset` above the assembly's bottom.
                let y = bottom - offset * ctx.scale + ink.y_min * ctx.scale;
                items.push(ctx.glyph_item(g, x, y));
            }
            (height, advance)
        }
    }
}

/// A horizontally stretched glyph on the baseline from x = 0: its width along the stretch,
/// its ascent and descent from the ink.
fn layout_stretched_horizontal(ctx: &Ctx, stretched: &Stretched) -> MathBox {
    match stretched {
        Stretched::Glyph(g) => MathBox {
            italic_correction: 0.0,
            ..natural_glyph_box(ctx, *g)
        },
        Stretched::Assembly { parts, extent } => {
            let mut out = MathBox {
                width: extent * ctx.scale,
                ..MathBox::default()
            };
            for &(g, offset) in parts {
                out.items.push(ctx.glyph_item(g, offset * ctx.scale, 0.0));
                if let Some(ink) = ctx.font.ink_box(g) {
                    out.ascent = out.ascent.max(ink.y_max * ctx.scale);
                    out.descent = out.descent.max(-ink.y_min * ctx.scale);
                }
            }
            out
        }
    }
}

/// An embellished operator's core `<mo>`: the node itself, the base of a script or
/// under/over, or what a style scope or one-child row holds (MathML Core's embellished
/// operator, less the space-like-siblings case).
fn core_operator(node: &Node) -> Option<(&str, &crate::ast::OperatorAttrs)> {
    match node {
        Node::Operator { text, attrs } => Some((text, attrs)),
        Node::Scripts { base, .. } | Node::UnderOver { base, .. } => core_operator(base),
        Node::Styled { children, .. } | Node::Row(children) if children.len() == 1 => {
            core_operator(&children[0])
        }
        _ => None,
    }
}

/// A node that may be a lone large operator: in display style the base of scripts or limits
/// takes its DisplayOperatorMinHeight variant. Style scopes around it are looked through and
/// put back.
fn layout_operator_base(ctx: &Ctx, node: &Node) -> MathBox {
    match node {
        Node::Operator { text, attrs } if ctx.display_style => {
            let flags = dictionary_entry(text, Form::Infix).2;
            match single_char(text) {
                Some(c) if attrs.largeop.unwrap_or(flags & opdict::LARGEOP != 0) => {
                    layout_large_operator(ctx, c)
                }
                _ => layout_node(ctx, node),
            }
        }
        Node::Styled { styles, children } if children.len() == 1 => {
            let inner = ctx.styled_child(styles);
            let core = layout_operator_base(&inner, &children[0]);
            decorate(&inner, styles, core)
        }
        _ => layout_node(ctx, node),
    }
}

/// A vertically stretchy operator over the row's extent: a symmetric one (a fence) grows
/// evenly about the math axis, another covers ascent and descent as they are. `minsize` and
/// `maxsize` clamp the target; a variant taller than the target is centred on it.
fn layout_stretchy_operator(
    ctx: &Ctx,
    c: char,
    symmetric: bool,
    (minsize, maxsize): (Option<Length>, Option<Length>),
    max_ascent: f32,
    max_descent: f32,
) -> MathBox {
    // The row checked the glyph exists; an empty box rather than trust that at a distance.
    let Some(glyph) = ctx.font.glyph_index(c) else {
        return MathBox::default();
    };
    let axis = ctx.constant(Constant::AxisHeight);
    let (mut target, mut target_ascent) = if symmetric {
        let above = (max_ascent - axis).max(max_descent + axis).max(0.0);
        (2.0 * above, axis + above)
    } else {
        ((max_ascent + max_descent).max(0.0), max_ascent)
    };
    if minsize.is_some() || maxsize.is_some() {
        let clamped = target
            .max(minsize.map_or(0.0, |l| ctx.resolve(l)))
            .min(maxsize.map_or(f32::INFINITY, |l| ctx.resolve(l)))
            .max(0.0);
        if symmetric {
            target_ascent = axis + clamped / 2.0;
        } else {
            // The change shared evenly above and below.
            target_ascent += (clamped - target) / 2.0;
        }
        target = clamped;
    }
    if target <= 0.0 {
        // Nothing to cover (a row of stretchy operators only): the natural glyph.
        return natural_glyph_box(ctx, ctx.script_glyph(glyph));
    }
    let stretched = ctx.font.stretch_vertical(glyph, target / ctx.scale);
    let (height, _) = emit_stretched(ctx, &stretched, 0.0, 0.0, &mut Vec::new());
    let ascent = target_ascent + (height - target).max(0.0) / 2.0;
    let mut out = MathBox::default();
    let (height, advance) = emit_stretched(ctx, &stretched, 0.0, -ascent, &mut out.items);
    out.width = advance;
    out.ascent = ascent;
    out.descent = height - ascent;
    out
}

/// A large operator (∫, ∑, …) in display style: the variant that reaches
/// DisplayOperatorMinHeight, centred on the math axis.
fn layout_large_operator(ctx: &Ctx, c: char) -> MathBox {
    let Some(glyph) = ctx.font.glyph_index(c) else {
        return layout_text_run(ctx, &c.to_string());
    };
    let min_height = ctx.font.constant(Constant::DisplayOperatorMinHeight);
    let stretched = ctx.font.stretch_vertical(glyph, min_height);
    let axis = ctx.constant(Constant::AxisHeight);
    let (height, _) = emit_stretched(ctx, &stretched, 0.0, 0.0, &mut Vec::new());
    let ascent = axis + height / 2.0;
    let mut out = MathBox::default();
    let (height, advance) = emit_stretched(ctx, &stretched, 0.0, -ascent, &mut out.items);
    out.width = advance;
    out.ascent = ascent;
    out.descent = height - ascent;
    if let Stretched::Glyph(g) = stretched {
        out.italic_correction = ctx.font.italic_correction(g) * ctx.scale;
        out.lone_glyph = Some(g);
    }
    out
}

/// What does not count as an operand when an operator's form is inferred (MathML Core's
/// space-like).
fn is_space_like(node: &Node) -> bool {
    match node {
        Node::Space { .. } | Node::Text(_) => true,
        Node::Row(children) | Node::Styled { children, .. } => children.iter().all(is_space_like),
        _ => false,
    }
}

/// The form by position: first of several operands prefix, last postfix, else (a lone child
/// too) infix.
fn infer_form(index: usize, significant: &[usize]) -> Form {
    if significant.len() > 1 {
        if significant.first() == Some(&index) {
            return Form::Prefix;
        }
        if significant.last() == Some(&index) {
            return Form::Postfix;
        }
    }
    Form::Infix
}

/// The dictionary entry, falling back as the spec does (the form asked for, then infix,
/// postfix, prefix) to the default: 5/18 em each side and no properties. An operator of
/// several characters is not in the dictionary and takes the default.
fn dictionary_entry(text: &str, form: Form) -> (u8, u8, u8) {
    let Some(c) = single_char(text) else {
        return (5, 5, 0);
    };
    let requested = match form {
        Form::Infix => opdict::FORM_INFIX,
        Form::Prefix => opdict::FORM_PREFIX,
        Form::Postfix => opdict::FORM_POSTFIX,
    };
    [
        requested,
        opdict::FORM_INFIX,
        opdict::FORM_POSTFIX,
        opdict::FORM_PREFIX,
    ]
    .into_iter()
    .find_map(|f| opdict::lookup(c, f))
    .unwrap_or((5, 5, 0))
}

/// A run of token text, shaped by HarfBuzz (kerning; `ssty` in scripts).
fn layout_text_run(ctx: &Ctx, text: &str) -> MathBox {
    let mut out = MathBox::default();
    let shaped = ctx.font.shape(text, ctx.script_level);
    let mut glyphs = shaped.iter().peekable();
    while let Some(g) = glyphs.next() {
        // Characters the font lacks go to the caller's fallback together, without .notdef's
        // advance, so a CJK `\text{}` is drawn as the system draws it.
        if g.id == 0 {
            while glyphs.next_if(|g| g.id == 0).is_some() {}
            let end = glyphs.peek().map_or(text.len(), |g| g.cluster);
            if let Some(missing) = text.get(g.cluster..end) {
                fall_back(ctx, missing, &mut out);
            }
            continue;
        }
        let x = out.width + g.x_offset * ctx.scale;
        // HarfBuzz offsets point up; the display list's y points down.
        let y = -g.y_offset * ctx.scale;
        // One HarfBuzz hides is left out: U+2061–U+2064 (`\sin x`'s function application)
        // shape to an inkless glyph without advance, which would only lend the run the font's
        // line height.
        if g.x_advance == 0.0 && ctx.font.ink_box(g.id).is_none() {
            continue;
        }
        let gid = ctx.script_glyph(g.id);
        // An alternate has an advance of its own; the shaped one is the replaced glyph's.
        out.width += if gid == g.id {
            g.x_advance
        } else {
            ctx.font.advance(gid)
        } * ctx.scale;
        out.items.push(ctx.glyph_item(gid, x, y));
        out.italic_correction = ctx.font.italic_correction(gid) * ctx.scale;
        if let Some(ink) = ctx.font.ink_box(gid) {
            out.ascent = out.ascent.max(ink.y_max * ctx.scale - y);
            out.descent = out.descent.max(y - ink.y_min * ctx.scale);
        }
    }
    // A run without ink (spaces only) still takes a line: font-wide metrics, so the box does
    // not collapse.
    if !out.items.is_empty() && out.ascent == 0.0 && out.descent == 0.0 {
        let (ascent, descent) = ctx.font.line_metrics();
        out.ascent = ascent * ctx.scale;
        out.descent = descent * ctx.scale;
    }
    if let [Item::Glyph { id, .. }] = out.items.as_slice() {
        out.lone_glyph = Some(*id);
    }
    out
}

/// `text`, characters the font lacks, onto the end of `out` as the caller's fallback measures
/// it; left out without one.
fn fall_back(ctx: &Ctx, text: &str, out: &mut MathBox) {
    let Some((width, ascent, descent)) = ctx.font.fallback(text, ctx.size) else {
        return;
    };
    out.items.push(Item::Text {
        text: text.to_string(),
        x: out.width,
        y: 0.0,
        size: ctx.size,
        color: ctx.color,
    });
    out.width += width;
    out.ascent = out.ascent.max(ascent);
    out.descent = out.descent.max(descent);
    out.italic_correction = 0.0;
}
