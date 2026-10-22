// Derived from formulary 0.3.0 `tests/golden.rs` (MIT OR Apache-2.0, Copyright (c) 2026 Jeffrey
// T. Peckham (Ophymx)), modified for accent; see crates/math/NOTICE.
//! Display lists against formulary 0.3.0's for the same MathML, recorded from it with read-fonts,
//! skrifa and harfrust (`UPDATE_GOLDEN=1` rewrites them), and the properties that pin the font
//! accessors HarfBuzz now answers.

use accent_math::{Item, Layout, MathFont, STIX_TWO_MATH, layout, parse};

/// Name and MathML: formulary's own golden formulas, written as pulldown-latex would mean the
/// same: `mstyle` as the `mrow` it gives `\displaystyle`, the matrix with the class it gives
/// `pmatrix`, and the arrow over `abc` said not to be an accent, which `\vec` would make it.
const GOLDENS: &[(&str, &str)] = &[
    ("simple_row", "<math><mi>x</mi><mo>+</mo><mn>2</mn></math>"),
    (
        "nested_mrow",
        "<math><mrow><mn>3</mn><mo>&#x2212;</mo><mrow><mi>a</mi><mi>b</mi></mrow></mrow></math>",
    ),
    (
        "mfrac_inline",
        "<math><mfrac><mn>1</mn><mrow><mi>x</mi><mo>+</mo><mn>2</mn></mrow></mfrac></math>",
    ),
    (
        "mfrac_display",
        r#"<math display="block"><mfrac><mn>1</mn><mrow><mi>x</mi><mo>+</mo><mn>2</mn></mrow></mfrac></math>"#,
    ),
    ("msup", "<math><msup><mi>x</mi><mn>2</mn></msup></math>"),
    (
        "msubsup",
        "<math><msubsup><mi>x</mi><mi>i</mi><mn>2</mn></msubsup><mo>+</mo><msub><mi>y</mi><mn>0</mn></msub></math>",
    ),
    (
        "msqrt",
        "<math><msqrt><mi>x</mi><mo>+</mo><mn>1</mn></msqrt></math>",
    ),
    (
        "msqrt_tall",
        r#"<math display="block"><msqrt><mfrac><mn>1</mn><mi>x</mi></mfrac></msqrt></math>"#,
    ),
    ("mroot", "<math><mroot><mi>x</mi><mn>3</mn></mroot></math>"),
    (
        "spacing_and_style",
        r#"<math><mi>a</mi><mspace width="1em"/><mrow displaystyle="true"><mfrac><mn>1</mn><mn>2</mn></mfrac></mrow></math>"#,
    ),
    (
        "quadratic_formula",
        r#"<math display="block"><mi>x</mi><mo>=</mo><mfrac><mrow><mo>&#x2212;</mo><mi>b</mi><mo>&#xB1;</mo><msqrt><msup><mi>b</mi><mn>2</mn></msup><mo>&#x2212;</mo><mn>4</mn><mi>a</mi><mi>c</mi></msqrt></mrow><mrow><mn>2</mn><mi>a</mi></mrow></mfrac></math>"#,
    ),
    (
        "stretchy_parens",
        r#"<math display="block"><mo>(</mo><mfrac><mn>1</mn><mn>2</mn></mfrac><mo>)</mo></math>"#,
    ),
    (
        "integral_display",
        r#"<math display="block"><mo>&#x222B;</mo><msup><mi>x</mi><mn>2</mn></msup><mspace width="0.17em"/><mi>d</mi><mi>x</mi></math>"#,
    ),
    (
        "sum_limits",
        r#"<math display="block"><munderover><mo>&#x2211;</mo><mrow><mi>n</mi><mo>=</mo><mn>1</mn></mrow><mi>N</mi></munderover><msup><mi>n</mi><mn>2</mn></msup></math>"#,
    ),
    (
        "accent_and_arrow",
        r#"<math><mover accent="true"><mi>x</mi><mo>&#x302;</mo></mover><mo>+</mo><mover accent="false"><mrow><mi>a</mi><mi>b</mi><mi>c</mi></mrow><mo>&#x2192;</mo></mover></math>"#,
    ),
    (
        "integral_bounds",
        r#"<math display="block"><msubsup><mo>&#x222B;</mo><mn>0</mn><mn>1</mn></msubsup><mi>x</mi><mspace width="0.17em"/><mi>d</mi><mi>x</mi></math>"#,
    ),
    (
        "matrix",
        r#"<math><mo>(</mo><mtable class="menv-arraylike"><mtr><mtd><mi>a</mi></mtd><mtd><mi>b</mi></mtd></mtr><mtr><mtd><mi>c</mi></mtd><mtd><mi>d</mi></mtd></mtr></mtable><mo>)</mo></math>"#,
    ),
    (
        "binomial",
        r#"<math><mo>(</mo><mfrac linethickness="0"><mi>n</mi><mi>k</mi></mfrac><mo>)</mo></math>"#,
    ),
];

fn font() -> MathFont {
    MathFont::from_static(STIX_TWO_MATH).unwrap()
}

fn laid(mathml: &str) -> Layout {
    let root = parse(mathml).unwrap();
    assert!(root.warnings.is_empty(), "{mathml}: {:?}", root.warnings);
    layout(&root, &font(), 16.0)
}

/// One line for the extents, `w a d`, then one per item: `g <id> <x> <y> <size> <colour|->`
/// or `r <x> <y> <w> <h> <colour|->`, to 0.01.
fn dump(l: &Layout) -> String {
    let n = |v: f32| match format!("{v:.2}") {
        z if z == "-0.00" => "0.00".to_string(),
        s => s,
    };
    let c = |c: Option<accent_math::Color>| {
        c.map_or("-".into(), |c| {
            format!("#{:02x}{:02x}{:02x}{:02x}", c.r, c.g, c.b, c.a)
        })
    };
    let mut out = format!("{} {} {}\n", n(l.width), n(l.ascent), n(l.descent));
    for item in &l.items {
        out += &match *item {
            Item::Glyph {
                id,
                x,
                y,
                size,
                color,
            } => format!("g {id} {} {} {} {}\n", n(x), n(y), n(size), c(color)),
            Item::Rule { x, y, w, h, color } => {
                format!("r {} {} {} {} {}\n", n(x), n(y), n(w), n(h), c(color))
            }
            Item::Text {
                ref text,
                x,
                y,
                size,
                color,
            } => format!("t {text} {} {} {} {}\n", n(x), n(y), n(size), c(color)),
        };
    }
    out
}

/// Every line has the same words, and every number is within 0.01 of the golden's.
fn agree(got: &str, want: &str) -> bool {
    let close = |a: &str, b: &str| match (a.parse::<f32>(), b.parse::<f32>()) {
        (Ok(a), Ok(b)) => (a - b).abs() <= 0.0101,
        _ => a == b,
    };
    got.lines().count() == want.lines().count()
        && got.lines().zip(want.lines()).all(|(g, w)| {
            let (g, w): (Vec<_>, Vec<_>) = (g.split(' ').collect(), w.split(' ').collect());
            g.len() == w.len() && g.iter().zip(&w).all(|(a, b)| close(a, b))
        })
}

#[test]
fn display_lists_match_the_goldens() {
    let font = font();
    let mut failed = Vec::new();
    for (name, mathml) in GOLDENS {
        let got = dump(&layout(&parse(mathml).unwrap(), &font, 32.0));
        let path = format!("{}/tests/golden/{name}.txt", env!("CARGO_MANIFEST_DIR"));
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::write(&path, &got).unwrap();
            continue;
        }
        let want = std::fs::read_to_string(&path).unwrap();
        if !agree(&got, &want) {
            failed.push(format!("{name}, got:\n{got}want:\n{want}"));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

fn glyphs(l: &Layout) -> Vec<(u16, f32, f32)> {
    l.items
        .iter()
        .filter_map(|i| match *i {
            Item::Glyph { id, x, y, .. } => Some((id, x, y)),
            _ => None,
        })
        .collect()
}

fn height(l: &Layout) -> f32 {
    l.ascent + l.descent
}

#[test]
fn scripts_use_ssty_alternates() {
    let alone = glyphs(&laid("<math><mo>&#x2032;</mo></math>"));
    let scripted = glyphs(&laid(
        "<math><msup><mi>f</mi><mo>&#x2032;</mo></msup></math>",
    ));
    assert_ne!(
        alone[0].0, scripted[1].0,
        "a prime in a script takes its ssty alternate"
    );
}

#[test]
fn math_kern_tucks_scripts() {
    // Italic V has a deep bottom-right cut: its subscript tucks well inside the advance
    // (10.24 px at 16 px) instead of hanging off the end.
    let g = glyphs(&laid("<math><msub><mi>V</mi><mn>2</mn></msub></math>"));
    assert!(
        g[1].1 < 8.0,
        "the subscript cuts into the V: x = {}",
        g[1].1
    );
    // The top-right kern of f pushes its superscript right of the bare advance.
    let g = glyphs(&laid("<math><msup><mi>f</mi><mn>2</mn></msup></math>"));
    assert!(g[1].1 > g[0].1);
}

#[test]
fn accents_use_attachment_points() {
    // The stretched hat over italic x lines up the font's attachment points: right of where
    // centring would put the wide variant (about -3.6), as italic x attaches right of centre.
    let g = glyphs(&laid(
        r#"<math><mover accent="true"><mi>x</mi><mo>&#x302;</mo></mover></math>"#,
    ));
    assert_eq!(g.len(), 2);
    assert!(
        g[1].1 > 0.0 && g[1].1 < 2.0,
        "the hat sits at the attachment point: x = {}",
        g[1].1
    );
}

#[test]
fn stretchy_parens_cover_content() {
    let small = laid("<math><mo>(</mo><mi>x</mi><mo>)</mo></math>");
    let tall = laid("<math><mo>(</mo><mfrac><mn>1</mn><mn>2</mn></mfrac><mo>)</mo></math>");
    // Around tall content the paren is another, taller glyph.
    assert_ne!(glyphs(&small)[0].0, glyphs(&tall)[0].0);
    // A symmetric fence covers both ends of the content.
    let frac = laid("<math><mfrac><mn>1</mn><mn>2</mn></mfrac></math>");
    assert!(tall.ascent >= frac.ascent - 1e-3);
    assert!(tall.descent >= frac.descent - 1e-3);
    // Content taller than every variant makes an assembly of several parts.
    let huge = laid(r#"<math><mo>(</mo><mspace height="120px" depth="120px"/><mo>)</mo></math>"#);
    let n = glyphs(&huge).len();
    assert!(n > 3, "assembled fences, got {n} glyphs");
    assert!(height(&huge) >= 240.0 - 1e-3);
}

#[test]
fn largeop_grows_in_display_style() {
    let inline = laid("<math><mo>&#x222B;</mo><mi>x</mi></math>");
    let display = laid(r#"<math display="block"><mo>&#x222B;</mo><mi>x</mi></math>"#);
    assert!(height(&display) > 1.5 * height(&inline));
    let opted_out =
        laid(r#"<math display="block"><mo largeop="false">&#x222B;</mo><mi>x</mi></math>"#);
    assert!(height(&opted_out) < height(&display));
}

#[test]
fn minsize_maxsize_clamp_stretching() {
    // minsize makes a tall fence around short content.
    let forced = laid(r#"<math><mo minsize="3em">(</mo><mi>x</mi><mo minsize="3em">)</mo></math>"#);
    let natural = laid("<math><mo>(</mo><mi>x</mi><mo>)</mo></math>");
    assert!(height(&forced) >= 44.0); // about 3 em at 16 px, less slack
    assert!(height(&forced) > 1.5 * height(&natural));

    // maxsize keeps a fence small around tall content: no taller than the fraction.
    let capped = laid(
        r#"<math><mo maxsize="1em">(</mo><mfrac><mn>1</mn><mn>2</mn></mfrac><mo maxsize="1em">)</mo></math>"#,
    );
    let uncapped = laid("<math><mo>(</mo><mfrac><mn>1</mn><mn>2</mn></mfrac><mo>)</mo></math>");
    let frac = laid("<math><mfrac><mn>1</mn><mn>2</mn></mfrac></math>");
    assert!(height(&capped) <= height(&frac) + 1e-3);
    assert!(height(&uncapped) > height(&capped));
}
