//! TeX as a note writes it, through the MathML `accent_core::markdown::mathml` makes of it:
//! pulldown-latex's dialect understood in full, and no formula that breaks the layout.

use accent_math::{Color, Item, Layout, MathFont, STIX_TWO_MATH, layout, parse};

fn font() -> MathFont {
    MathFont::from_static(STIX_TWO_MATH).unwrap()
}

/// `tex` laid out at 16 px, which pulldown-latex must take and this crate understand in full.
fn laid(tex: &str, display: bool) -> Layout {
    let mathml = accent_core::markdown::mathml(tex, display)
        .unwrap_or_else(|| panic!("pulldown-latex rejects {tex}"));
    let root = parse(&mathml).unwrap_or_else(|e| panic!("{tex}: {e}\n{mathml}"));
    assert!(
        root.warnings.is_empty(),
        "{tex}: {:?}\n{mathml}",
        root.warnings
    );
    layout(&root, &font(), 16.0)
}

/// Every extent finite and not negative, every coordinate finite.
fn sane(l: &Layout) -> bool {
    let extents = [l.width, l.ascent, l.descent];
    extents.iter().all(|v| v.is_finite() && *v >= 0.0)
        && l.items.iter().all(|i| match *i {
            Item::Glyph { x, y, size, .. } => [x, y, size].iter().all(|v| v.is_finite()),
            Item::Rule { x, y, w, h, .. } => [x, y, w, h].iter().all(|v| v.is_finite()),
        })
}

fn glyphs(l: &Layout) -> Vec<(f32, f32)> {
    l.items
        .iter()
        .filter_map(|i| match *i {
            Item::Glyph { x, size, .. } => Some((x, size)),
            Item::Rule { .. } => None,
        })
        .collect()
}

fn rules(l: &Layout) -> usize {
    l.items
        .iter()
        .filter(|i| matches!(i, Item::Rule { .. }))
        .count()
}

/// `\boxed` is not in pulldown-latex 0.8: `\fcolorbox` stands in for a box.
const FORMULAS: &[&str] = &[
    r"x^2",
    r"\frac{a}{b}",
    r"\sqrt[3]{x}",
    r"\sum_{i=1}^n",
    r"\int_0^1 f\,dx",
    r"\left(\frac ab\right)",
    r"\big( x \big)",
    r"\text{ for all }",
    r"\hat x \vec v \overline{AB}",
    r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}",
    r"\begin{cases} 1 & x > 0 \\ 0 & \text{otherwise} \end{cases}",
    r"\begin{align} a &= b \\[4pt] c &= d \\ \end{align}",
    r"\begin{array}{c|c} a & b \\ \hline c & d \end{array}",
    r"\color{red}{x}",
    r"\fcolorbox{red}{yellow}{x}",
    r"\not\in",
    r"\mathbb R",
    r"\sin x",
    r"2\sin^2 x",
    r"a\!b",
    r"\binom nk",
    r"\overbrace{a+b}^n",
    r"\displaystyle \sum_{i=1}^n i",
    r"a < b \& c",
    r"\underline{x} \underbrace{a+b}_n",
    r"a \mskip 18mu b",
    r"\shortmid x",
];

#[test]
fn a_notes_formulas_lay_out() {
    for tex in FORMULAS {
        for display in [false, true] {
            let l = laid(tex, display);
            assert!(!l.items.is_empty() && l.width > 0.0, "{tex}");
            assert!(sane(&l), "{tex}: {l:?}");
        }
    }
}

#[test]
fn an_accent_keeps_its_base_size() {
    let g = glyphs(&laid(r"\hat x", false));
    assert_eq!(g.len(), 2);
    assert_eq!(g[0].1, g[1].1, "the hat is as large as the x");
}

#[test]
fn align_puts_its_first_column_right() {
    let l = laid(r"\begin{aligned} a &= b \\ ccc &= d \end{aligned}", false);
    let g = glyphs(&l);
    let (a, c) = (laid("a", false).width, laid("c", false).width);
    // a, =, b, then c, c, c, =, d: the a ends where the third c does.
    assert!((g[0].0 + a - (g[5].0 + c)).abs() < 0.01, "{g:?}");
}

#[test]
fn color_inks_its_glyph() {
    let l = laid(r"\color{red}{x}", false);
    let red = Some(Color {
        r: 255,
        g: 0,
        b: 0,
        a: 255,
    });
    assert!(matches!(l.items[..], [Item::Glyph { color, .. }] if color == red));
}

#[test]
fn negative_space_pulls_back() {
    assert!(laid(r"a\!b", false).width < laid("ab", false).width);
}

#[test]
fn big_delimiters_are_taller() {
    let height = |l: Layout| l.ascent + l.descent;
    assert!(height(laid(r"\big(", false)) > height(laid("(", false)));
}

/// The function application after `\sin` takes no room: x sits where a plain thin space puts it.
#[test]
fn a_function_name_leaves_no_gap() {
    let sin = laid(r"\sin x", false);
    let plain = layout(
        &parse(r#"<math><mi>sin</mi><mspace width="0.1667em"/><mi>x</mi></math>"#).unwrap(),
        &font(),
        16.0,
    );
    assert_eq!(sin, plain);
}

/// Bars, lines and boxes are rules: over- and underlines one each, an array's vertical line one
/// per row and its `\hline` one, and a box its four sides and its background.
#[test]
fn lines_are_rules() {
    assert_eq!(rules(&laid(r"\overline{AB}", false)), 1);
    assert_eq!(rules(&laid(r"\underline{x}", false)), 1);
    let array = r"\begin{array}{c|c} a & b \\ \hline c & d \end{array}";
    assert_eq!(rules(&laid(array, false)), 3);
    assert_eq!(rules(&laid(r"\fcolorbox{red}{yellow}{x}", false)), 5);
}

/// splitmix64, gen-vault's generator: the same formulas every run, without a dependency.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) % n as u64) as usize
    }
}

/// What the generator builds formulas of; a `#` takes another formula.
const FRAGMENTS: &[&str] = &[
    "x",
    "2",
    "a+b",
    "#^{#}",
    "x_{#}",
    "x_i^{#}",
    r"\frac{#}{#}",
    r"\dfrac{#}{y}",
    r"\tfrac{1}{#}",
    r"\sqrt{#}",
    r"\sqrt[3]{#}",
    r"\left( # \right)",
    r"\left[ # \right]",
    r"\left\{ # \right.",
    r"\left\langle # \middle| # \right\rangle",
    r"\big( # \big)",
    r"\Bigg| # \Bigg|",
    r"\sum_{i=1}^{n} #",
    r"\int_0^1 # \,dx",
    r"\prod_{k} #",
    r"\lim_{x \to 0} #",
    r"\iint #",
    r"\hat{#}",
    r"\widehat{#}",
    r"\vec{#}",
    r"\overline{#}",
    r"\underline{#}",
    r"\tilde{#}",
    r"\dot{#}",
    r"\overbrace{#}^{n}",
    r"\underbrace{#}_{k}",
    r"\overrightarrow{#}",
    r"\xrightarrow{#}",
    r"\text{ if } #",
    r"\mathbb{R} \mathbf{v} \mathcal{L} \mathrm{d}",
    r"\sin #",
    r"\log #",
    r"\alpha\beta\gamma \infty \partial \nabla",
    r"a < b \le c \ne #",
    r"\not\in \not\subset",
    r"a \& b",
    r"a\,b\;c\!d\quad #",
    r"\mskip 9mu \hspace{1cm} \!\!\!",
    r"\color{red}{#}",
    r"\fcolorbox{red}{yellow}{#}",
    r"\colorbox{yellow}{#}",
    r"\binom{#}{k}",
    r"\displaystyle #",
    r"\scriptstyle #",
    r"\begin{pmatrix} # & 0 \\ 0 & 1 \end{pmatrix}",
    r"\begin{bmatrix} # \end{bmatrix}",
    r"\begin{cases} # & x > 0 \\ 0 & \text{otherwise} \end{cases}",
    r"\begin{aligned} a &= # \\ &= b \end{aligned}",
    r"\begin{align} # \\[4pt] x &= y \\ \end{align}",
    r"\begin{array}{|c||c|} # & b \\ \hline\hline c & d \end{array}",
    r"\begin{array}{l:r} # & b \\ \hdashline c & d \end{array}",
    r"\begin{gathered} # \\ y \end{gathered}",
    r"\substack{# \\ j}",
    r"f'(x) + g''",
    r"\{x \mid #\}",
    r"\shortmid \smallint",
    r"\text{中} #",
];

fn formula(rng: &mut Rng, depth: u32) -> String {
    (0..=rng.below(3))
        .map(|_| {
            let fragment = FRAGMENTS[rng.below(FRAGMENTS.len())];
            let mut pieces = fragment.split('#');
            let mut out = pieces.next().unwrap_or_default().to_string();
            for piece in pieces {
                out += &if depth < 3 {
                    formula(rng, depth + 1)
                } else {
                    "z".to_string()
                };
                out += piece;
            }
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// About 2,000 formulas pieced together at random: whatever pulldown-latex takes is XML this
/// crate understands in full and lays out without a panic, into sane extents.
#[test]
fn generated_formulas_never_break_the_layout() {
    let font = font();
    let mut rng = Rng(2026);
    let mut taken = 0;
    for i in 0..2000 {
        let tex = formula(&mut rng, 0);
        let Some(mathml) = accent_core::markdown::mathml(&tex, i % 2 == 0) else {
            continue;
        };
        let root = parse(&mathml).unwrap_or_else(|e| panic!("{tex}: {e}\n{mathml}"));
        assert!(
            root.warnings.is_empty(),
            "{tex}: {:?}\n{mathml}",
            root.warnings
        );
        let l = layout(&root, &font, 16.0);
        assert!(sane(&l), "{tex}: {l:?}");
        taken += 1;
    }
    assert!(taken > 1500, "pulldown-latex took only {taken} of 2000");
}
