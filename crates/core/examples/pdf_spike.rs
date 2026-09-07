//! Phase 0 spike C: prove the core can render, select, theme and read highlights out of a PDF.
//!
//! ```text
//! cargo run --release -p accent-core --features pdf --example pdf_spike -- <file.pdf> [page]
//! ```
//!
//! Writes `page-light.ppm` / `page-dark.ppm` next to `$ACCENT_SCRATCH` (default: the cwd) and
//! prints a timing for each step.

use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use accent_core::pdf::{self, PdfDoc, Rect, RgbaImage, Selection, Theme};
use anyhow::{Result, bail};

const SCALE: f32 = 2.0;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        bail!("usage: pdf_spike <file.pdf> [page]");
    };
    let page: usize = args.next().map(|p| p.parse()).transpose()?.unwrap_or(0);
    let out_dir =
        PathBuf::from(std::env::var("ACCENT_SCRATCH").unwrap_or_else(|_| ".".to_string()));

    println!("pdfium dir : {}", accent_core::pdf::library_dir().display());
    if !accent_core::pdf::available() {
        bail!("no libpdfium found; see accent_core::pdf::library_dir()");
    }

    let t = Instant::now();
    let doc = PdfDoc::open(&path)?;
    let (w, h) = doc.page_size(page)?;
    println!(
        "open       : {:>7.1} ms  ({} pages, page {page} is {w:.1} x {h:.1} pt)",
        ms(t),
        doc.page_count()
    );

    // --- render, light + dark ------------------------------------------------------------
    let t = Instant::now();
    let light = doc.render_page(page, SCALE, Theme::Plain)?;
    let light_ms = ms(t);
    println!(
        "render x{SCALE} : {light_ms:>7.1} ms  ({} x {} px)",
        light.width, light.height
    );

    // Time the dark post-process on its own, over the buffer we already have, rather than
    // subtracting two renders from each other.
    let mut dark = RgbaImage {
        width: light.width,
        height: light.height,
        data: light.data.clone(),
    };
    // libadwaita's dark view colours, which is what the app asks for under a dark theme.
    let (paper, ink) = ([0x1d, 0x1d, 0x20], [0xeb, 0xeb, 0xeb]);
    let t = Instant::now();
    for px in dark.data.chunks_exact_mut(4) {
        px.copy_from_slice(&accent_core::pdf::recolour_pixel(
            [px[0], px[1], px[2], px[3]],
            paper,
            ink,
        ));
    }
    println!(
        "  dark pass: {:>7.1} ms  ({} px)",
        ms(t),
        light.width * light.height
    );

    write_ppm(&out_dir.join("page-light.ppm"), &light)?;
    write_ppm(&out_dir.join("page-dark.ppm"), &dark)?;
    println!("wrote      : {}/page-{{light,dark}}.ppm", out_dir.display());

    // --- text ----------------------------------------------------------------------------
    let t = Instant::now();
    let glyphs = doc.page_text(page)?;
    println!("text       : {:>7.1} ms  ({} glyphs)", ms(t), glyphs.len());
    let text: String = glyphs.iter().map(|g| g.ch).collect();
    println!(
        "  first 200: {:?}",
        text.chars().take(200).collect::<String>()
    );
    if let Some(g) = glyphs.first() {
        println!("  glyph[0] : {:?} at {:?}", g.ch, g.rect);
    }

    let t = Instant::now();
    let top_third = Rect {
        left: 0.0,
        top: 0.0,
        right: w,
        bottom: h / 3.0,
    };
    let region = doc.text_in_rect(page, top_third);
    println!(
        "in-rect    : {:>7.1} ms  {:?}",
        ms(t),
        region.chars().take(60).collect::<String>()
    );

    // --- selection link ------------------------------------------------------------------
    let t = Instant::now();
    let rel = PathBuf::from(&path)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or(path.clone());
    let sel = Selection {
        page,
        start: 0,
        end: 40.min(glyphs.len()),
    };
    let link = pdf::selection_link(&glyphs, &rel, &sel);
    println!("selection  : {:>7.1} ms", ms(t));
    println!("  link     : {}", link.link);
    println!("  text     : {:?}", link.text);
    println!("  quads    : {:?}", link.quads);

    // --- highlights ----------------------------------------------------------------------
    let t = Instant::now();
    let highlights = doc.highlights()?;
    println!(
        "highlights : {:>7.1} ms  ({} found)",
        ms(t),
        highlights.len()
    );
    for hl in highlights.iter().take(10) {
        println!(
            "  p{} rgba{:?} quads={} {:?}",
            hl.page,
            hl.color,
            hl.quads.len(),
            hl.contents
        );
        if let Some(q) = hl.quads.first() {
            println!("      first quad {q:?} corners {:?}", q.quad_corners());
        }
    }

    Ok(())
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// Binary PPM (P6). No image crate for a spike that only needs eyeballs on the output.
fn write_ppm(path: &std::path::Path, img: &RgbaImage) -> Result<()> {
    let mut buf = Vec::with_capacity(img.data.len() / 4 * 3 + 32);
    buf.extend_from_slice(format!("P6\n{} {}\n255\n", img.width, img.height).as_bytes());
    for px in img.data.chunks_exact(4) {
        buf.extend_from_slice(&px[..3]);
    }
    std::fs::File::create(path)?.write_all(&buf)?;
    Ok(())
}
