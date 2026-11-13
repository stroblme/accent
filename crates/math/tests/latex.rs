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
    assert!(!leaks(&mathml), "{tex}: {mathml}");
    let root = parse(&mathml).unwrap_or_else(|e| panic!("{tex}: {e}\n{mathml}"));
    assert!(
        root.warnings.is_empty(),
        "{tex}: {:?}\n{mathml}",
        root.warnings
    );
    layout(&root, &font(), 16.0)
}

/// Whether TeX shows as text, as `\mathop{\times}` showed `\times`: a backslash before a letter
/// outside the text `\text{}` and `\colorbox{}` write as it is.
fn leaks(mathml: &str) -> bool {
    mathml
        .split("<mtext")
        .map(|s| s.split_once("</mtext>").map_or(s, |(_, math)| math))
        .any(|math| {
            math.as_bytes()
                .windows(2)
                .any(|w| w[0] == b'\\' && w[1].is_ascii_alphabetic())
        })
}

/// Every extent finite and not negative, every coordinate finite.
fn sane(l: &Layout) -> bool {
    let extents = [l.width, l.ascent, l.descent];
    extents.iter().all(|v| v.is_finite() && *v >= 0.0)
        && l.items.iter().all(|i| match *i {
            Item::Glyph { x, y, size, .. } => [x, y, size].iter().all(|v| v.is_finite()),
            Item::Rule { x, y, w, h, .. } => [x, y, w, h].iter().all(|v| v.is_finite()),
            Item::Line { x, y, x2, y2, .. } => [x, y, x2, y2].iter().all(|v| v.is_finite()),
            Item::Text { x, y, size, .. } => [x, y, size].iter().all(|v| v.is_finite()),
        })
}

fn glyphs(l: &Layout) -> Vec<(f32, f32)> {
    l.items
        .iter()
        .filter_map(|i| match *i {
            Item::Glyph { x, size, .. } => Some((x, size)),
            _ => None,
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
    r"a \not{a+b} c",
    r"\begin{multline} a + b \\ c \\ d + e + f \end{multline}",
    r"\begin{array}{c|c} a & b \\ \hline\hline c & d \end{array}",
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
    r"\mathop{\times} \mathop{\sum}\limits_a^b \mathop{\mathbb{E}}_x",
    r"a \mathbin{\times} b \mathrel{\to} \mathopen{\{} c \mathclose{\}}",
    r"\operatorname*{arg\,max}_x f \operatorname{a<b}",
    r"\DeclareMathOperator{\rank}{rank} \rank A",
    r"\sin\limits_x",
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

/// `\hat` keeps the mark's natural glyph over a wide base, as TeX draws it; `\widehat` stretches.
#[test]
fn only_wide_accents_stretch() {
    let mark = |tex| match laid(tex, false).items.last() {
        Some(Item::Glyph { id, .. }) => *id,
        other => panic!("{tex}: {other:?}"),
    };
    assert_eq!(mark(r"\hat{abc}"), mark(r"\hat x"));
    assert_ne!(mark(r"\widehat{abc}"), mark(r"\hat x"));
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

/// A character the math font lacks is the caller's to measure and draw, and is left out where
/// the caller has no fallback.
#[test]
fn missing_characters_fall_back() {
    let mathml = accent_core::markdown::mathml(r"\text{a中b}", false).unwrap();
    let root = parse(&mathml).unwrap();
    let without = layout(&root, &font(), 16.0);
    let mut fallback = font();
    fallback.set_fallback(|text, size| (text == "中").then_some((size, 0.9 * size, 0.1 * size)));
    let with = layout(&root, &fallback, 16.0);
    assert!((with.width - without.width - 16.0).abs() < 0.01);
    assert!(with.ascent >= 14.4 && with.descent >= 1.6);
    let a = laid(r"\text{a}", false).width;
    assert!(
        matches!(&with.items[..], [Item::Glyph { .. }, Item::Text { text, x, .. }, Item::Glyph { .. }]
            if text == "中" && (x - a).abs() < 0.01),
        "{with:?}"
    );
}

/// `\not` before a group strikes it through from its bottom-left corner to its top-right.
#[test]
fn a_negated_group_is_struck_through() {
    let l = laid(r"\not{a+b}", false);
    let close = |a: f32, b: f32| (a - b).abs() < 0.01;
    assert!(
        matches!(l.items.last(), Some(&Item::Line { x, y, x2, y2, .. })
            if close(x, 0.0) && close(y, l.descent) && close(x2, l.width) && close(y2, -l.ascent)),
        "{l:?}"
    );
}

/// `multline` sets its first line left, its last right and the one between in the middle.
#[test]
fn multline_steps_its_lines() {
    let l = laid(r"\begin{multline} a+b+c+d \\ e \\ f \end{multline}", true);
    let g = glyphs(&l);
    let (x0, xe, xf) = (g[0].0, g[7].0, g[8].0);
    let (e, f) = (laid("e", false).width, laid("f", false).width);
    assert!(x0.abs() < 0.01, "{g:?}");
    assert!((xe + e / 2.0 - l.width / 2.0).abs() < 0.01, "{g:?}");
    assert!((xf + f - l.width).abs() < 0.01, "{g:?}");
}

/// Two `\hline`s are 0.1 em apart, the row between them for room only.
#[test]
fn a_doubled_hline_is_two_close_rules() {
    let l = laid(r"\begin{array}{c} a \\ \hline\hline b \end{array}", true);
    let ys: Vec<f32> = l
        .items
        .iter()
        .filter_map(|i| match *i {
            Item::Rule { y, h, w, .. } if w > h => Some(y),
            _ => None,
        })
        .collect();
    assert_eq!(ys.len(), 2, "{l:?}");
    assert!((ys[1] - ys[0] - 1.6).abs() < 0.01, "{ys:?}");
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

/// A delimiter alone in a group takes its own size and not the row's: `{\Big(}` is as tall as
/// `\Big(`, and a grouped `\vert` beside a fraction keeps the glyph it has alone.
#[test]
fn a_grouped_delimiter_keeps_its_own_size() {
    let height = |l: Layout| l.ascent + l.descent;
    assert_eq!(
        height(laid(r"{\Big(}", false)),
        height(laid(r"\Big(", false))
    );
    let bar = |tex| match laid(tex, false).items[0] {
        Item::Glyph { id, .. } => id,
        ref other => panic!("{tex}: {other:?}"),
    };
    assert_eq!(bar(r"{\vert}\frac{a}{b}"), bar(r"\vert"));
}

/// `\displaystyle`, which SymPy starts every formula with, sets a large operator large.
#[test]
fn display_style_enlarges_an_integral() {
    let height = |l: Layout| l.ascent + l.descent;
    assert!(height(laid(r"\displaystyle \int x", false)) > height(laid(r"\int x", false)));
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

/// A symbol command in a class command is the character it stands for, which that command sets
/// as MathJax does: `\mathop{\times}` is the operator ×.
#[test]
fn a_class_command_takes_a_symbol_command() {
    assert_eq!(laid(r"\mathop{\times}", false), laid(r"\mathop{×}", false));
    assert_eq!(
        laid(r"a\mathbin{\times}b", false),
        laid(r"a\times b", false)
    );
    assert_eq!(
        laid(r"a\mathrel{\vert}b", false),
        laid(r"a\mathrel{|}b", false)
    );
    for display in [false, true] {
        assert_eq!(
            laid(r"\mathop{\sum}_a^b", display),
            laid(r"\sum_a^b", display)
        );
    }
    // A sized one keeps its size.
    let sized = accent_core::markdown::mathml(r"\mathopen{\big(}", false).unwrap();
    assert!(sized.contains("minsize"), "{sized}");
}

/// A class command that gives a symbol or a group another class spaces it as TeX spaces that
/// class between ordinary atoms, where MathML would space it by the operator dictionary: thick
/// round a relation, a run of symbols such as `:=` set close, the trailing space after the
/// scripts, and none on a side with nothing to space it from. One operator, decorated, keeps the
/// space MathML gives its core.
#[test]
fn a_class_command_spaces_by_its_class() {
    let same = |tex, spaced| assert_eq!(laid(tex, false), laid(spaced, false), "{tex}");
    same(r"a\mathrel{\times}b", r"a\;\mathord{\times}\;b");
    same(r"a\mathrel{:=}b", r"a\;\mathord{:}\mathord{=}\;b");
    same(r"a\mathbin{xy}b", r"a\:xy\:b");
    same(r"a\mathinner{\ldots}b", r"a\,\mathord{\ldots}\,b");
    same(r"a\mathrel{\times}_1b", r"a\;{\mathord{\times}}_1\;b");
    same(r"(\mathrel{\times}b)", r"(\mathord{\times}\;b)");
    same(r"a\mathrel{\overset{!}{=}}b", r"a\overset{!}{=}b");
    let width = |tex| laid(tex, false).width;
    assert!(width(r"a\mathrel{\times}b") > width(r"a\mathbin{\times}b"));
    assert!(width(r"a\mathrel{:=}b") < width(r"a:=b"));
}

/// An operator's limits go above and below it in display or after `\limits`, and after it inline
/// or after `\nolimits`, `\operatorname` keeping them after it unless starred.
#[test]
fn an_operator_places_its_limits() {
    let width = |tex, display| laid(tex, display).width;
    let op = r"\mathop{\times}_a^b";
    assert!(width(op, true) < width(op, false));
    assert_eq!(laid(r"\mathop{\times}\limits_a^b", false), laid(op, true));
    assert_eq!(laid(r"\mathop{\times}\nolimits_a^b", true), laid(op, false));
    assert!(width(r"\sin\limits_x", false) < width(r"\sin_x", false));
    let name = r"\operatorname{arg\,max}_x";
    assert!(width(r"\operatorname*{arg\,max}_x", true) < width(name, true));
    assert_eq!(laid(name, true), laid(name, false));
}

/// An operator's name is set upright, a space and all, and `\DeclareMathOperator` names it.
#[test]
fn an_operator_name_is_upright() {
    assert_eq!(laid(r"\operatorname{E}", false), laid(r"\mathrm{E}", false));
    assert_eq!(
        laid(r"\operatorname{arg\,max}", false),
        laid(r"\mathrm{arg\,max}", false)
    );
    for display in [false, true] {
        assert_eq!(
            laid(
                r"\DeclareMathOperator*{\argmax}{arg\,max} \argmax_x f",
                display
            ),
            laid(r"\operatorname*{arg\,max}_x f", display)
        );
    }
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
    r"\mathop{#}_{x}",
    r"\operatorname*{#}_{i}",
    r"a \mathbin{#} \mathrel{\leq} b",
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
        assert!(!leaks(&mathml), "{tex}: {mathml}");
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
