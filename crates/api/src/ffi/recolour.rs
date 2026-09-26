//! Images recoloured like a PDF page, and the test for which ones want it.
//!
//! The rules are the core's ([`accent_core::recolour`]): Android decodes with its own decoder and
//! hands the pixels over as straight-alpha RGBA8, in the same [`Theme`] a PDF page is rendered
//! in, so a figure and a page in one vault land on the same paper.

use accent_core::pdf;
use accent_core::recolour;

use crate::ffi::convert::Theme;

/// Whether `width` × `height` straight-alpha RGBA8 pixels read as a document — a scan, plot,
/// diagram or screenshot of text — rather than a photo, and so want recolouring. A buffer that
/// does not match its size, or an image above 64 MP, is never one. See [`recolour::classify`].
#[uniffi::export]
pub fn looks_like_document(rgba: Vec<u8>, width: u32, height: u32) -> bool {
    recolour::classify(&rgba, width, height).document
}

/// Straight-alpha RGBA8 pixels moved onto the theme's paper and ink, alpha untouched; under
/// [`Theme::Plain`] they come back as they went in.
#[uniffi::export]
pub fn recolour_image(mut rgba: Vec<u8>, theme: Theme) -> Vec<u8> {
    if let pdf::Theme::Recolour { paper, ink } = theme.into() {
        recolour::recolour(&mut rgba, paper, ink);
    }
    rgba
}

/// An SVG's text with the same recolouring as a filter, so it stays vector; under
/// [`Theme::Plain`] it comes back as it went in. `None` when there is no root `<svg>` element to
/// hang the filter on.
#[uniffi::export]
pub fn recolour_svg(svg: String, theme: Theme) -> Option<String> {
    match theme.into() {
        pdf::Theme::Plain => Some(svg),
        pdf::Theme::Recolour { paper, ink } => recolour::recolour_svg(&svg, paper, ink),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Adwaita dark pair, as Kotlin spells it: `0xRRGGBB`.
    fn dark() -> Theme {
        Theme::Recolour {
            paper: 0x1d1d20,
            ink: 0xebebeb,
        }
    }

    /// The thresholds are the core's and tested there; what is tested here is that a page and a
    /// photo get told apart through the ffi's own types.
    #[test]
    fn a_page_is_a_document_and_noise_is_not() {
        let mut page = [255u8; 64 * 64 * 4];
        page[..64 * 8 * 4].fill(0); // a black band over the top eighth
        assert!(looks_like_document(page.to_vec(), 64, 64));

        let noise: Vec<u8> = (0..64 * 64u32)
            .flat_map(|i| {
                let [r, g, b, _] = i.wrapping_mul(2_654_435_761).to_be_bytes();
                [r, g, b, 255]
            })
            .collect();
        assert!(!looks_like_document(noise, 64, 64));
        assert!(!looks_like_document(vec![255; 8], 64, 64), "short buffer");
    }

    #[test]
    fn an_image_lands_on_the_theme_paper_unless_plain() {
        let white = vec![255, 255, 255, 255, 0, 0, 0, 0];
        assert_eq!(recolour_image(white.clone(), Theme::Plain), white);
        let out = recolour_image(white, dark());
        assert!(
            out[..3]
                .iter()
                .zip([0x1d, 0x1d, 0x20])
                .all(|(&a, b)| a.abs_diff(b) <= 1)
        );
        assert_eq!((out[3], out[7]), (255, 0), "alpha untouched");
    }

    #[test]
    fn an_svg_gets_the_filter_unless_plain() {
        let svg =
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><path d=\"M0 0H9\"/></svg>".to_string();
        assert_eq!(recolour_svg(svg.clone(), Theme::Plain), Some(svg.clone()));
        let out = recolour_svg(svg, dark()).unwrap();
        assert!(out.contains("<feColorMatrix type=\"matrix\""), "{out}");
        assert_eq!(recolour_svg("not a drawing".into(), dark()), None);
    }
}
