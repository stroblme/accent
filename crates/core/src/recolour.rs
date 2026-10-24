//! Theme recolouring shared by PDF pages, document images and SVGs. White maps to paper and
//! black to ink while retaining chroma. Raster buffers use [`recolour`]; SVGs keep vector
//! content through [`recolour_svg`] and [`colour_matrix`].
//!
//! [`onto`] combines theme and inversion preferences; [`classify`] distinguishes document images
//! from photos by light neutral paper and limited colours. Apps decode images; this module
//! consumes straight-alpha RGBA8.

/// Rec. 709 luma weights, taken on the sRGB values as they are rather than on linear light: the
/// ramp needs an order from ink to paper, not a measurement.
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// A page's paper and ink, the two ends of the ramp a file is put on.
pub type Page = ([u8; 3], [u8; 3]);

/// The page a file is shown on, or `None` to show it as it is. `own` is the page of the theme's
/// half on screen and `other` the other half's, `None` for a half that leaves files alone (Light).
///
/// `auto` is whether the file's kind is recoloured unasked (Preferences). If it is, the file goes
/// onto the theme's page and an inverted one onto the other half's; if not, the file shows as it
/// is and an inverted one goes onto the theme's page.
pub fn onto(auto: bool, inverted: bool, own: Option<Page>, other: Option<Page>) -> Option<Page> {
    match (auto, inverted) {
        (true, false) | (false, true) => own,
        (true, true) => other,
        (false, false) => None,
    }
}

// ---------------------------------------------------------------------------------------------
// The remap
// ---------------------------------------------------------------------------------------------

/// Move a pixel onto the theme's paper–ink ramp while keeping its chroma, so white paper becomes
/// `paper` and black text becomes `ink`, but a yellow highlight stays yellow.
///
/// The pixel's luminance picks a point on the ramp; the pixel's own offset from its luminance is
/// then added back per channel, which is what carries the colour across.
///
// ponytail: this keeps the *absolute* chroma offset `c - luminance` and clamps, which is one
// multiply-free pass over the buffer and good enough to read by. The ceiling: saturated colours
// near the ends of the ramp lose some saturation to the clamp, and it is not a real perceptual
// space. Upgrade path when someone complains is Oklab — convert, remap L, convert back — at
// roughly 3x the cost, at which point this wants SIMD or the GPU.
pub fn recolour_pixel(px: [u8; 4], paper: [u8; 3], ink: [u8; 3]) -> [u8; 4] {
    let (r, g, b) = (
        px[0] as f32 / 255.0,
        px[1] as f32 / 255.0,
        px[2] as f32 / 255.0,
    );
    let l = LUMA[0] * r + LUMA[1] * g + LUMA[2] * b;
    let map = |i: usize, c: f32| {
        let (ink, paper) = (ink[i] as f32 / 255.0, paper[i] as f32 / 255.0);
        let target = ink + l * (paper - ink);
        ((target + (c - l)) * 255.0).round().clamp(0.0, 255.0) as u8
    };
    [map(0, r), map(1, g), map(2, b), px[3]]
}

/// [`LUMA`] in 65536ths, rounded so the three still sum to one: white is exactly the top of the
/// ramp.
const LUMA_Q16: [u32; 3] = [13_933, 46_871, 4_732];

/// The steps of the ramp [`recolour`] looks up, a sixteenth of a byte level each.
const STEPS: i32 = 255 * 16;

/// Recolour RGBA8 in place, shared by page renders, tiles and decoded images. Integer luma and
/// ramp lookups avoid per-pixel float arithmetic, agreeing with [`recolour_pixel`] within one
/// byte level and exactly at paper and ink endpoints.
pub fn recolour(data: &mut [u8], paper: [u8; 3], ink: [u8; 3]) {
    // In bytes and rounded: `ink + y·(paper − ink − 255) / STEPS`.
    let ramp: Vec<[i16; 3]> = (0..=STEPS)
        .map(|y| {
            [0, 1, 2].map(|i| {
                let (paper, ink) = (i32::from(paper[i]), i32::from(ink[i]));
                let at = ink * STEPS + y * (paper - ink - 255);
                (at + STEPS / 2).div_euclid(STEPS) as i16
            })
        })
        .collect();
    for px in data.as_chunks_mut::<4>().0 {
        let luma: u32 = (0..3).map(|i| LUMA_Q16[i] * u32::from(px[i])).sum();
        let offset = ramp[((luma + 2048) >> 12) as usize];
        for i in 0..3 {
            px[i] = (i16::from(px[i]) + offset[i]).clamp(0, 255) as u8;
        }
    }
}

/// Composite straight-alpha RGBA8 over white, in place, leaving it opaque: a transparent figure
/// flattened onto the page it was drawn for, before [`recolour`] turns that white into paper. For
/// a page that is not the window's own, where the window showing through would be the wrong
/// colour — a remap is affine, so this is the same as recolouring and then filling with paper.
pub fn onto_white(data: &mut [u8]) {
    for px in data.as_chunks_mut::<4>().0 {
        let a = u32::from(px[3]);
        for c in &mut px[..3] {
            *c = 255 - (((255 - u32::from(*c)) * a + 127) / 255) as u8;
        }
        px[3] = 255;
    }
}

/// Affine [`recolour_pixel`] matrix: four row-major rows of five values in 0..1 units, offset
/// last, matching `feColorMatrix`.
///
/// For luma `Y`, the remap is `c + ink + (paper − ink − 1)·Y`. Each row adds its ramp
/// coefficient times the luma weights to the identity and offsets by ink; alpha passes through.
/// Float rounding agrees within one byte level.
pub fn colour_matrix(paper: [u8; 3], ink: [u8; 3]) -> [f32; 20] {
    let mut m = [0.0; 20];
    for i in 0..3 {
        let (paper, ink) = (f32::from(paper[i]) / 255.0, f32::from(ink[i]) / 255.0);
        for (j, luma) in LUMA.iter().enumerate() {
            m[i * 5 + j] = (paper - ink - 1.0) * luma;
        }
        m[i * 5 + i] += 1.0;
        m[i * 5 + 4] = ink;
    }
    m[18] = 1.0;
    m
}

/// The [`colour_matrix`]-shaped matrix that takes a pixel as the remap onto `page` showed it to
/// what the file's own pixel, multiplied channel by channel by `by`, remaps to; `None` is a page
/// left alone, where it is the product itself. A highlighter's `Multiply` stroke laid over a
/// page already on screen, so it shows as the render will leave it.
///
/// The remap is `A·c + ink`, `A = I + k·Lᵀ` with `k = paper − ink − 1` and `L` the luma weights,
/// so this is `A·diag(by)·A⁻¹` offset by `ink` less its own image of `ink`, `A⁻¹` being
/// Sherman–Morrison's `I − k·Lᵀ / (1 + L·k)`. Exact where the remap did not clamp.
pub fn multiplied(page: Option<Page>, by: [f32; 3]) -> [f32; 20] {
    let unit = |v: [u8; 3]| v.map(|c| f32::from(c) / 255.0);
    let (k, ink) = match page {
        Some((paper, ink)) => {
            let (paper, ink) = (unit(paper), unit(ink));
            ([0, 1, 2].map(|i| paper[i] - ink[i] - 1.0), ink)
        }
        None => ([0.0; 3], [0.0; 3]),
    };
    let lk: f32 = (0..3).map(|i| LUMA[i] * k[i]).sum();
    let identity = |i: usize, j: usize| f32::from(u8::from(i == j));
    let remap = |i: usize, j: usize| identity(i, j) + k[i] * LUMA[j];
    let unmap = |i: usize, j: usize| identity(i, j) - k[i] * LUMA[j] / (1.0 + lk);
    let mut m = [0.0; 20];
    for i in 0..3 {
        for j in 0..3 {
            m[i * 5 + j] = (0..3).map(|n| remap(i, n) * by[n] * unmap(n, j)).sum();
        }
    }
    for i in 0..3 {
        m[i * 5 + 4] = ink[i] - (0..3).map(|j| m[i * 5 + j] * ink[j]).sum::<f32>();
    }
    m[18] = 1.0;
    m
}

// ---------------------------------------------------------------------------------------------
// Which images are documents
// ---------------------------------------------------------------------------------------------

/// Above this many pixels an image is left alone without being looked at: 256 MB decoded, which
/// a second recoloured copy would double. A camera's 50 MP still fits under it.
pub const MAX_PIXELS: u64 = 64_000_000;

/// About how many pixels [`classify`] samples: a share measured on 65 536 of them is right to
/// within a percent, and the walk costs nothing next to decoding even a 64 MP image.
const SAMPLES: u64 = 65_536;

/// Paper is at least this light. A scan's page, a yellowed one and a plot's light-grey panel
/// (ggplot's `#ebebeb` is 0.92) stay above it; a photo's midtones do not.
const PAPER_LUMA: f32 = 0.80;

/// And at most this colourful, as max − min of the channels out of 255, about 15 %. Cream paper
/// (Solarized's `#fdf6e3` is 26) and draw.io's pale blue and green fills (`#dae8fc` is 34) pass;
/// its pale orange (`#ffe6cc`, 51) and a clear sky (`#87ceeb`, 100) do not.
const PAPER_CHROMA: u8 = 38;

/// A document is mostly its paper: a page of text is a few percent ink, a plot's lines less.
/// Below half, the image is about something else.
const PAPER_SHARE: f32 = 0.50;

/// The share of samples the colour count has to cover. The last tenth is left out because that
/// is where anti-aliasing lives: the edge of every glyph and line spreads over the bins between
/// ink and paper, few pixels each.
const COVERAGE: f32 = 0.90;

/// A document has paper, ink and a legend's worth of colours, each over a bin or two at 4 bits a
/// channel, where a bin is 16 levels wide and swallows a scan's noise. A photo's shading fills
/// hundreds of bins even that coarse.
const MAX_COLOURS: u32 = 48;

/// What [`classify`] measured on an image, and what it concluded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Verdict {
    /// Share of the samples that are paper: light and near-neutral.
    pub paper: f32,
    /// How many 4-bit-per-channel colour bins, of 4096, it takes to hold 90 % of the samples.
    pub colours: u32,
    /// Mostly paper, in few colours: a document, to be recoloured like a PDF page.
    pub document: bool,
}

/// Whether an image reads as a document — a scan, plot, diagram or screenshot of text — rather
/// than a photo, from its straight-alpha RGBA8 pixels.
///
/// Every sample is composited over white first, the page a transparent figure was drawn for, so
/// black line art on nothing is a document. An empty image, one whose buffer does not match its
/// size, and one above [`MAX_PIXELS`] are not measured and never a document.
pub fn classify(rgba: &[u8], width: u32, height: u32) -> Verdict {
    let pixels = u64::from(width) * u64::from(height);
    if pixels == 0 || pixels > MAX_PIXELS || rgba.len() as u64 != pixels * 4 {
        return Verdict {
            paper: 0.0,
            colours: 0,
            document: false,
        };
    }
    let mut bins = vec![0u32; 1 << 12];
    let (mut samples, mut paper) = (0u32, 0u32);
    for i in grid(width, height) {
        let px = &rgba[i * 4..i * 4 + 4];
        let alpha = u32::from(px[3]);
        let [r, g, b] =
            [px[0], px[1], px[2]].map(|c| (255 - (255 - u32::from(c)) * alpha / 255) as u8);
        let luma =
            (LUMA[0] * f32::from(r) + LUMA[1] * f32::from(g) + LUMA[2] * f32::from(b)) / 255.0;
        let chroma = r.max(g).max(b) - r.min(g).min(b);
        if luma >= PAPER_LUMA && chroma <= PAPER_CHROMA {
            paper += 1;
        }
        bins[(usize::from(r >> 4) << 8) | (usize::from(g >> 4) << 4) | usize::from(b >> 4)] += 1;
        samples += 1;
    }

    bins.sort_unstable_by(|a, b| b.cmp(a));
    let wanted = (samples as f32 * COVERAGE).ceil() as u32;
    let (mut held, mut colours) = (0, 0);
    for n in bins {
        if held >= wanted {
            break;
        }
        held += n;
        colours += 1;
    }

    let paper = paper as f32 / samples as f32;
    Verdict {
        paper,
        colours,
        document: paper >= PAPER_SHARE && colours <= MAX_COLOURS,
    }
}

/// The pixel indices [`classify`] samples: a square grid, as coarse as it has to be for about
/// [`SAMPLES`] of them.
fn grid(width: u32, height: u32) -> impl Iterator<Item = usize> {
    let (w, h) = (width as usize, height as usize);
    let step = ((w * h) as f64 / SAMPLES as f64).sqrt().ceil().max(1.0) as usize;
    (0..h)
        .step_by(step)
        .flat_map(move |y| (0..w).step_by(step).map(move |x| y * w + x))
}

// ---------------------------------------------------------------------------------------------
// SVG
// ---------------------------------------------------------------------------------------------

/// `svg` with [`colour_matrix`] over everything it draws, as a filter, so it stays vector.
/// `None` when there is no root `<svg …>` start tag to hang the filter on, or the root is empty
/// (`<svg/>`).
///
/// The root's content moves into one `<g>` carrying the filter. `color-interpolation-filters`
/// is `sRGB` so the matrix sees the values [`recolour_pixel`] does; the default would linearise
/// them first. The filter region is left at its default, the group's bounds and 10 % a side,
/// which holds a stroke's overhang past its path; one sized to the canvas would need the root's
/// `viewBox` parsed.
pub fn recolour_svg(svg: &str, paper: [u8; 3], ink: [u8; 3]) -> Option<String> {
    filtered(svg, paper, ink, false)
}

/// [`recolour_svg`] on a page of the drawing's own, as [`onto_white`] does for a raster: a white
/// backdrop under everything it draws, filtered with it into the paper. It covers the root's
/// `viewBox`, or the viewport without one; a viewport wider than its box keeps a clear margin.
pub fn recolour_svg_on_paper(svg: &str, paper: [u8; 3], ink: [u8; 3]) -> Option<String> {
    filtered(svg, paper, ink, true)
}

fn filtered(svg: &str, paper: [u8; 3], ink: [u8; 3], backdrop: bool) -> Option<String> {
    let (start, open) = root_content(svg)?;
    let close = svg.rfind("</svg").filter(|&c| c >= open)?;
    let values: Vec<String> = colour_matrix(paper, ink)
        .iter()
        .map(f32::to_string)
        .collect();
    let backdrop = match (backdrop, view_box(&svg[start..open])) {
        (false, _) => String::new(),
        (true, Some([x, y, w, h])) => {
            format!("<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" fill=\"white\"/>")
        }
        (true, None) => "<rect width=\"100%\" height=\"100%\" fill=\"white\"/>".to_string(),
    };
    Some(format!(
        "{}<filter id=\"accent-recolour\" color-interpolation-filters=\"sRGB\">\
         <feColorMatrix type=\"matrix\" values=\"{}\"/></filter>\
         <g filter=\"url(#accent-recolour)\">{backdrop}{}</g>{}",
        &svg[..open],
        values.join(" "),
        &svg[open..close],
        &svg[close..],
    ))
}

/// The four numbers of a start tag's `viewBox`, read back as numbers so nothing but a number is
/// written into the backdrop.
fn view_box(tag: &str) -> Option<[f64; 4]> {
    let rest = tag.split_once("viewBox=")?.1;
    let quote = rest.chars().next().filter(|&q| q == '"' || q == '\'')?;
    let numbers: Vec<f64> = rest[1..]
        .split(quote)
        .next()?
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|n| !n.is_empty())
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    numbers.try_into().ok()
}

/// Where the root element's start tag begins, and where its content starts: just past that tag,
/// a `>` inside a quoted attribute not counting. `None` without one, or for an empty `<svg …/>`.
fn root_content(svg: &str) -> Option<(usize, usize)> {
    let bytes = svg.as_bytes();
    let (start, _) = svg.match_indices("<svg").find(|(i, _)| {
        matches!(
            bytes.get(i + 4),
            Some(b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>')
        )
    })?;
    let mut quote = None;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return (bytes[i - 1] != b'/').then_some((start, i + 1)),
            None => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // The three pairs `theme.rs` asks for: the Adwaita dark view with its foreground, and each
    // half of Solarized. Light leaves a page alone.
    const ADWAITA_DARK: ([u8; 3], [u8; 3]) = ([0x1d, 0x1d, 0x20], [0xeb, 0xeb, 0xeb]);
    const SOLARIZED_LIGHT: ([u8; 3], [u8; 3]) = ([0xfd, 0xf6, 0xe3], [0x65, 0x7b, 0x83]);
    const SOLARIZED_DARK: ([u8; 3], [u8; 3]) = ([0x00, 0x2b, 0x36], [0x83, 0x94, 0x96]);
    const PALETTES: [([u8; 3], [u8; 3]); 3] = [ADWAITA_DARK, SOLARIZED_LIGHT, SOLARIZED_DARK];

    /// Inverting flips whether a file is recoloured: onto the other half's page where its kind is
    /// recoloured, onto the theme's own where it is left alone. Light leaves it alone unless it
    /// is inverted with its kind recoloured.
    #[test]
    fn inverting_recolours_a_kind_left_alone_onto_the_theme() {
        let (own, other) = (Some(SOLARIZED_DARK), Some(SOLARIZED_LIGHT));
        assert_eq!(onto(true, false, own, other), own);
        assert_eq!(onto(true, true, own, other), other);
        assert_eq!(onto(false, false, own, other), None);
        assert_eq!(onto(false, true, own, other), own);

        let light = (None, Some(ADWAITA_DARK));
        assert_eq!(onto(true, false, light.0, light.1), None);
        assert_eq!(onto(true, true, light.0, light.1), light.1);
        assert_eq!(onto(false, false, light.0, light.1), None);
        assert_eq!(onto(false, true, light.0, light.1), None);
    }

    /// Exactly, not within a step: white paper that lands a shade off the window's own colour
    /// is a seam around every page.
    #[test]
    fn recolour_maps_paper_and_ink_exactly_to_the_theme() {
        for (paper, ink) in PALETTES {
            let [r, g, b, a] = recolour_pixel([255, 255, 255, 255], paper, ink);
            assert_eq!(([r, g, b], a), (paper, 255), "white, alpha preserved");
            let [r, g, b, _] = recolour_pixel([0, 0, 0, 255], paper, ink);
            assert_eq!([r, g, b], ink, "black");
        }
    }

    #[test]
    fn onto_white_flattens_transparency_and_leaves_the_opaque_alone() {
        let mut data = vec![0, 0, 0, 0, 0, 0, 0, 128, 10, 20, 30, 255];
        onto_white(&mut data);
        assert_eq!(
            data,
            [255, 255, 255, 255, 127, 127, 127, 255, 10, 20, 30, 255]
        );
    }

    #[test]
    fn recolour_keeps_a_saturated_colour_recognisable() {
        for (paper, ink) in [ADWAITA_DARK, SOLARIZED_LIGHT] {
            // A yellow highlight must remain a yellow highlight, not turn grey.
            let yellow = recolour_pixel([255, 255, 0, 255], paper, ink);
            assert!(
                yellow[0] > yellow[2] + 40 && yellow[1] > yellow[2] + 40,
                "{yellow:?} on paper {paper:?}"
            );

            let red = recolour_pixel([255, 0, 0, 255], paper, ink);
            assert!(
                red[0] > red[1] + 40 && red[0] > red[2] + 40,
                "stays reddish: {red:?} on paper {paper:?}"
            );
        }
    }

    #[test]
    fn recolour_runs_the_pixel_remap_over_a_buffer() {
        let (paper, ink) = ADWAITA_DARK;
        let mut data = vec![255, 255, 255, 255, 0, 0, 0, 128];
        recolour(&mut data, paper, ink);
        assert_eq!(data[..4], recolour_pixel([255, 255, 255, 255], paper, ink));
        assert_eq!(data[4..], recolour_pixel([0, 0, 0, 128], paper, ink));
    }

    /// The buffer's integer path against the pixel's float one, over every fifth level of each
    /// channel: never more than a step of a byte apart, and white and black exactly paper and ink.
    #[test]
    fn recolour_is_the_pixel_remap_within_a_step() {
        let levels = || (0..=255u8).step_by(5);
        let pixels: Vec<[u8; 4]> = levels()
            .flat_map(|r| levels().flat_map(move |g| levels().map(move |b| [r, g, b, 200])))
            .collect();
        for (paper, ink) in PALETTES {
            let mut data = pixels.concat();
            recolour(&mut data, paper, ink);
            for (&px, out) in pixels.iter().zip(data.as_chunks::<4>().0) {
                let want = recolour_pixel(px, paper, ink);
                let near = (0..4).all(|i| out[i].abs_diff(want[i]) <= 1);
                assert!(near, "{px:?} -> {out:?}, pixel path {want:?}");
            }
            let mut ends = [255, 255, 255, 255, 0, 0, 0, 255];
            recolour(&mut ends, paper, ink);
            let [p0, p1, p2] = paper;
            let [i0, i1, i2] = ink;
            assert_eq!(ends, [p0, p1, p2, 255, i0, i1, i2, 255]);
        }
    }

    /// Whatever applies the matrix — an SVG filter, a GPU node — paints what the CPU path would.
    #[test]
    fn the_colour_matrix_is_the_pixel_remap() {
        let levels = || (0..=255u8).step_by(17);
        for (paper, ink) in PALETTES {
            let m = colour_matrix(paper, ink);
            for r in levels() {
                for g in levels() {
                    for b in levels() {
                        let c = [r, g, b, 200].map(|v| f32::from(v) / 255.0);
                        let row = |i: usize| (0..4).map(|j| m[i * 5 + j] * c[j]).sum::<f32>();
                        let out = [0, 1, 2, 3]
                            .map(|i| ((row(i) + m[i * 5 + 4]).clamp(0.0, 1.0) * 255.0).round());
                        let want = recolour_pixel([r, g, b, 200], paper, ink);
                        for i in 0..4 {
                            assert!(
                                (out[i] - f32::from(want[i])).abs() <= 1.0,
                                "{:?} -> {out:?}, pixel path {want:?}",
                                [r, g, b]
                            );
                        }
                    }
                }
            }
        }
    }

    /// A highlighter over a page on screen lands where its render will: the file's pixel times
    /// the stroke, remapped, for every theme and for a page left alone. Pixels the remap clamps
    /// are left out, since what was clamped away cannot be read back off the screen.
    #[test]
    fn a_multiplied_pixel_on_screen_is_the_remapped_product() {
        // Accent blue and yellow at the highlighter's 0.4: what one pass over white leaves.
        let by = [[0x35, 0x84, 0xe4], [0xf6, 0xd3, 0x2d]]
            .map(|c: [u8; 3]| c.map(|v| 0.6 + 0.4 * f32::from(v) / 255.0));
        let apply = |m: &[f32; 20], c: [f32; 3]| {
            [0, 1, 2].map(|i| (0..3).map(|j| m[i * 5 + j] * c[j]).sum::<f32>() + m[i * 5 + 4])
        };
        let levels = || (0..=255u8).step_by(15);
        for page in [
            None,
            Some(ADWAITA_DARK),
            Some(SOLARIZED_LIGHT),
            Some(SOLARIZED_DARK),
        ] {
            let (remap, flat) = match page {
                Some((paper, ink)) => (colour_matrix(paper, ink), false),
                None => (multiplied(None, [1.0; 3]), true),
            };
            let shown = |c: [f32; 3]| apply(&remap, c);
            let inside = |c: [f32; 3]| c.iter().all(|v| (0.0..=1.0).contains(v));
            for by in by {
                let m = multiplied(page, by);
                for r in levels() {
                    for g in levels() {
                        for b in levels() {
                            let file = [r, g, b].map(|v| f32::from(v) / 255.0);
                            let product = [0, 1, 2].map(|i| file[i] * by[i]);
                            if !flat && !(inside(shown(file)) && inside(shown(product))) {
                                continue;
                            }
                            let got = apply(&m, shown(file));
                            let want = shown(product);
                            for i in 0..3 {
                                assert!(
                                    (got[i] - want[i]).abs() < 1.0 / 255.0,
                                    "{page:?} {:?} -> {got:?}, want {want:?}",
                                    [r, g, b]
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // ---- synthetic images for the classifier

    const WHITE: [u8; 4] = [255, 255, 255, 255];
    const BLACK: [u8; 4] = [0, 0, 0, 255];

    /// A straight-alpha RGBA8 canvas to draw test images on.
    struct Canvas {
        w: u32,
        h: u32,
        px: Vec<u8>,
    }

    impl Canvas {
        fn new(w: u32, h: u32, fill: [u8; 4]) -> Self {
            let px = fill.repeat((w * h) as usize);
            Canvas { w, h, px }
        }

        fn set(&mut self, x: u32, y: u32, c: [u8; 4]) {
            if x < self.w && y < self.h {
                let i = ((y * self.w + x) * 4) as usize;
                self.px[i..i + 4].copy_from_slice(&c);
            }
        }

        fn rect(&mut self, x0: u32, y0: u32, x1: u32, y1: u32, c: [u8; 4]) {
            for y in y0..y1 {
                for x in x0..x1 {
                    self.set(x, y, c);
                }
            }
        }

        /// A filled rectangle with a `line`-wide outline, as a diagram draws a box.
        fn boxed(&mut self, x0: u32, y0: u32, x1: u32, y1: u32, fill: [u8; 4], line: [u8; 4]) {
            self.rect(x0, y0, x1, y1, line);
            self.rect(x0 + 2, y0 + 2, x1 - 2, y1 - 2, fill);
        }

        /// A `thick`-pixel-wide curve through `y = f(x)` for every column from `x0` to `x1`.
        fn curve(&mut self, x0: u32, x1: u32, thick: u32, c: [u8; 4], f: impl Fn(f32) -> f32) {
            for x in x0..x1 {
                let y = f(x as f32) as u32;
                self.rect(x, y, x + 1, y + thick, c);
            }
        }

        fn verdict(&self) -> Verdict {
            classify(&self.px, self.w, self.h)
        }
    }

    /// Deterministic noise: Knuth's MMIX LCG, top bits.
    fn noise(seed: u64) -> impl FnMut() -> u8 {
        let mut s = seed;
        move || {
            s = s
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (s >> 56) as u8
        }
    }

    fn rgb(hex: u32) -> [u8; 4] {
        let [_, r, g, b] = hex.to_be_bytes();
        [r, g, b, 255]
    }

    /// A white page of text: lines of black words, each with a grey row above and below for the
    /// anti-aliasing.
    fn text_page() -> Canvas {
        let mut c = Canvas::new(600, 800, WHITE);
        for (line, y) in (60..740).step_by(24).enumerate() {
            let mut x = 60;
            let mut word = line;
            while x < 540 {
                let end = (x + 20 + (word * 37 % 50) as u32).min(540);
                c.rect(x, y, end, y + 1, rgb(0xaaaaaa));
                c.rect(x, y + 1, end, y + 10, BLACK);
                c.rect(x, y + 10, end, y + 11, rgb(0xaaaaaa));
                x = end + 8;
                word += 1;
            }
        }
        c
    }

    /// Black strokes on nothing, alpha-feathered at their edges: a circle and a cross.
    fn line_art() -> Canvas {
        let mut c = Canvas::new(400, 400, [0, 0, 0, 0]);
        for y in 0..400 {
            for x in 0..400 {
                let (dx, dy) = (x as f32 - 200.0, y as f32 - 200.0);
                let off = ((dx * dx + dy * dy).sqrt() - 150.0).abs();
                let off = off.min(dx.abs()).min(dy.abs());
                let alpha = (255.0 * (2.5 - off).clamp(0.0, 1.0)) as u8;
                if alpha > 0 {
                    c.set(x, y, [0, 0, 0, alpha]);
                }
            }
        }
        c
    }

    /// A white plot: black axes and three sines in matplotlib's first three colours.
    fn plot() -> Canvas {
        let mut c = Canvas::new(640, 480, WHITE);
        c.rect(60, 40, 62, 420, BLACK);
        c.rect(60, 418, 600, 420, BLACK);
        for (k, colour) in [0x1f77b4, 0xff7f0e, 0x2ca02c].into_iter().enumerate() {
            let phase = k as f32;
            c.curve(62, 600, 3, rgb(colour), |x| {
                230.0 + 150.0 * (x / 60.0 + phase).sin()
            });
        }
        c
    }

    /// draw.io's four pale fills in boxes with darker outlines, half the page between them, and
    /// black arrows between the boxes.
    fn diagram() -> Canvas {
        let mut c = Canvas::new(800, 600, WHITE);
        let boxes = [
            (40, 30, 0xdae8fc, 0x6c8ebf),
            (440, 30, 0xd5e8d4, 0x82b366),
            (40, 330, 0xffe6cc, 0xd79b00),
            (440, 330, 0xf8cecc, 0xb85450),
        ];
        for (x, y, fill, line) in boxes {
            c.boxed(x, y, x + 320, y + 188, rgb(fill), rgb(line));
        }
        c.rect(360, 122, 440, 126, BLACK);
        c.rect(198, 218, 202, 330, BLACK);
        c
    }

    /// Uniform noise: nothing but colour.
    fn noise_image() -> Canvas {
        let mut next = noise(1);
        let mut c = Canvas::new(256, 256, WHITE);
        for px in c.px.as_chunks_mut::<4>().0 {
            *px = [next(), next(), next(), 255];
        }
        c
    }

    /// A smooth two-dimensional sweep of colour, the nearest thing to a photo's shading.
    fn gradient() -> Canvas {
        let mut c = Canvas::new(512, 512, WHITE);
        for y in 0..512 {
            for x in 0..512 {
                let b = 255 - (x + y) / 4;
                c.set(x, y, [(x / 2) as u8, (y / 2) as u8, b as u8, 255]);
            }
        }
        c
    }

    /// A dark editor's screenshot: light text bars on `#1e1e1e`.
    fn dark_screenshot() -> Canvas {
        let mut c = Canvas::new(800, 600, rgb(0x1e1e1e));
        for y in (20..580).step_by(20) {
            c.rect(40, y, 40 + (y * 7 % 600), y + 8, rgb(0xd4d4d4));
        }
        c
    }

    /// A white canvas with a disc of noise over 45 % of it: paper enough, but in every colour.
    fn noisy_disc() -> Canvas {
        let mut next = noise(2);
        let mut c = Canvas::new(600, 600, WHITE);
        let r2 = 0.45 * 600.0 * 600.0 / std::f32::consts::PI;
        for y in 0..600 {
            for x in 0..600 {
                let (dx, dy) = (x as f32 - 300.0, y as f32 - 300.0);
                if dx * dx + dy * dy <= r2 {
                    c.set(x, y, [next(), next(), next(), 255]);
                }
            }
        }
        c
    }

    #[test]
    fn documents_are_documents() {
        for (name, image) in [
            ("text page", text_page()),
            ("line art", line_art()),
            ("plot", plot()),
            ("diagram", diagram()),
        ] {
            let v = image.verdict();
            eprintln!("{name}: {v:?}");
            assert!(v.document, "{name}: {v:?}");
        }
    }

    #[test]
    fn photos_and_dark_screens_are_not() {
        for (name, image) in [
            ("noise", noise_image()),
            ("gradient", gradient()),
            ("dark screenshot", dark_screenshot()),
            ("noisy disc", noisy_disc()),
        ] {
            let v = image.verdict();
            eprintln!("{name}: {v:?}");
            assert!(!v.document, "{name}: {v:?}");
        }
        // The disc is the colour count's case: the page alone is paper enough.
        let disc = noisy_disc().verdict();
        assert!(disc.paper >= PAPER_SHARE, "{disc:?}");
    }

    #[test]
    fn a_degenerate_buffer_is_never_a_document() {
        assert!(!classify(&[], 0, 0).document);
        assert!(!classify(&[255; 12], 2, 2).document, "short buffer");
        assert!(!classify(&[255; 20], 2, 2).document, "long buffer");
        let one = classify(&WHITE, 1, 1);
        assert_eq!((one.paper, one.colours), (1.0, 1), "{one:?}");
    }

    #[test]
    fn a_large_image_is_sampled_not_walked() {
        let n = grid(8000, 6000).count() as u64;
        assert!((SAMPLES / 2..=SAMPLES).contains(&n), "{n} samples");
        assert_eq!(
            grid(100, 100).count(),
            10_000,
            "a small image is read whole"
        );
    }

    // ---- SVG

    fn filtered(svg: &str) -> Option<String> {
        let (paper, ink) = ADWAITA_DARK;
        recolour_svg(svg, paper, ink)
    }

    #[test]
    fn an_svg_is_wrapped_in_the_filter() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10"/></svg>"#;
        let out = filtered(svg).unwrap();
        let head = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><filter id="accent-recolour" color-interpolation-filters="sRGB"><feColorMatrix type="matrix" values=""#;
        assert!(out.starts_with(head), "{out}");
        assert!(
            out.ends_with(
                r#"<g filter="url(#accent-recolour)"><rect width="10" height="10"/></g></svg>"#
            ),
            "{out}"
        );
        let values = out[head.len()..].split('"').next().unwrap();
        let (paper, ink) = ADWAITA_DARK;
        let want = colour_matrix(paper, ink);
        let got: Vec<f32> = values.split(' ').map(|v| v.parse().unwrap()).collect();
        assert_eq!(got, want, "{values}");
    }

    #[test]
    fn a_prolog_and_a_quoted_bracket_are_stepped_over() {
        let svg = "<?xml version=\"1.0\"?>\n\
            <!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"svg11.dtd\">\n\
            <!-- Created by hand -->\n\
            <svg viewBox=\"0 0 1 1\" data-note='a > b'>\n<path d=\"M0 0\"/>\n</svg>\n";
        let out = filtered(svg).unwrap();
        let root = "<svg viewBox=\"0 0 1 1\" data-note='a > b'>";
        let at = out.find(root).unwrap() + root.len();
        assert!(out[..at].starts_with("<?xml"), "{out}");
        assert!(
            out[at..].starts_with("<filter id=\"accent-recolour\""),
            "{out}"
        );
        assert!(out.ends_with("<path d=\"M0 0\"/>\n</g></svg>\n"), "{out}");
    }

    #[test]
    fn on_paper_an_svg_gets_a_white_backdrop_over_its_view_box() {
        let (paper, ink) = ADWAITA_DARK;
        let content = r#"<path d="M0 0"/>"#;
        let boxed = format!(r#"<svg viewBox="-5,10 320 200.5">{content}</svg>"#);
        let out = recolour_svg_on_paper(&boxed, paper, ink).unwrap();
        let backdrop = r#"<rect x="-5" y="10" width="320" height="200.5" fill="white"/>"#;
        assert!(
            out.contains(&format!(r#"(#accent-recolour)">{backdrop}{content}</g>"#)),
            "{out}"
        );
        // Without a viewBox the canvas is the viewport, in pixels from its corner.
        let bare = format!(r#"<svg width="10" height="10">{content}</svg>"#);
        let out = recolour_svg_on_paper(&bare, paper, ink).unwrap();
        assert!(
            out.contains(r#"<rect width="100%" height="100%" fill="white"/><path"#),
            "{out}"
        );
        // A viewBox that is not four numbers is no box to fill.
        let odd = format!(r#"<svg viewBox='0 0 1 "x"'>{content}</svg>"#);
        let out = recolour_svg_on_paper(&odd, paper, ink).unwrap();
        assert!(out.contains(r#"<rect width="100%""#), "{out}");
    }

    #[test]
    fn an_empty_or_missing_root_is_refused() {
        assert_eq!(
            filtered(r#"<svg xmlns="http://www.w3.org/2000/svg"/>"#),
            None
        );
        assert_eq!(filtered("<html><body>not a drawing</body></html>"), None);
        assert_eq!(filtered("<svgfoo></svgfoo>"), None);
        assert_eq!(filtered("<svg width=\"1\""), None, "unterminated tag");
    }
}
