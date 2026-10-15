//! Core recolouring, EXIF orientation, and animation detection for Android-decoded straight-alpha
//! RGBA8 pixels. Figures and pages share the PDF [`Theme`] ([`accent_core::recolour`],
//! [`accent_core::orientation`], [`accent_core::animation`]).

use accent_core::pdf;
use accent_core::recolour;
use accent_core::{animation, orientation};

use crate::ffi::convert::{Theme, Tile};

/// Classify `width` × `height` straight-alpha RGBA8 as recolourable document
/// content rather than a photo ([`recolour::classify`]). Invalid buffer sizes
/// and images above 64 MP return false.
#[uniffi::export]
pub fn looks_like_document(rgba: Vec<u8>, width: u32, height: u32) -> bool {
    recolour::classify(&rgba, width, height).document
}

/// Apply theme paper and ink to straight-alpha RGBA8. Preserve alpha unless
/// `fill` composites onto paper ([`recolour::onto_white`]); [`Theme::Plain`]
/// returns pixels unchanged.
#[uniffi::export]
pub fn recolour_image(mut rgba: Vec<u8>, theme: Theme, fill: bool) -> Vec<u8> {
    if let pdf::Theme::Recolour { paper, ink } = theme.into() {
        if fill {
            recolour::onto_white(&mut rgba);
        }
        recolour::recolour(&mut rgba, paper, ink);
    }
    rgba
}

/// Orient `width` × `height` straight-alpha RGBA8 using `path`'s EXIF tag,
/// which `BitmapFactory` ignores. Return unchanged pixels for absent tags
/// or invalid buffer sizes.
#[uniffi::export]
pub fn upright_image(rgba: Vec<u8>, width: u32, height: u32, path: String) -> Tile {
    let tag = std::fs::File::open(path).map_or(1, |mut file| orientation::read(&mut file));
    let (rgba, width, height) = match tag {
        tag if tag != 1 && rgba.len() == width as usize * height as usize * 4 => {
            orientation::apply(&rgba, width, height, tag)
        }
        _ => (rgba, width, height),
    };
    Tile {
        width,
        height,
        rgba,
    }
}

/// Whether `path` is an animated GIF, WebP or PNG, which `BitmapFactory`
/// decodes as one still. False for a still, any other file, or none.
#[uniffi::export]
pub fn is_animated(path: String) -> bool {
    std::fs::File::open(path).is_ok_and(animation::animated)
}

/// Add a recolouring filter to SVG text, preserving vectors. `fill` adds
/// paper ([`recolour::recolour_svg_on_paper`]); [`Theme::Plain`] is unchanged.
/// `None` means no root `<svg>` exists for the filter.
#[uniffi::export]
pub fn recolour_svg(svg: String, theme: Theme, fill: bool) -> Option<String> {
    match theme.into() {
        pdf::Theme::Plain => Some(svg),
        pdf::Theme::Recolour { paper, ink } if fill => {
            recolour::recolour_svg_on_paper(&svg, paper, ink)
        }
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

    /// Verify page/photo classification through FFI types; core tests cover thresholds.
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
        assert_eq!(recolour_image(white.clone(), Theme::Plain, true), white);
        let out = recolour_image(white.clone(), dark(), false);
        assert_eq!(
            out,
            [0x1d, 0x1d, 0x20, 255, 0xeb, 0xeb, 0xeb, 0],
            "alpha untouched"
        );
        let filled = recolour_image(white, dark(), true);
        assert_eq!(
            filled[4..],
            [0x1d, 0x1d, 0x20, 255],
            "the clear pixel is paper"
        );
    }

    /// Verify EXIF reaches FFI pixels and absent tags preserve them; core tests
    /// cover tag parsing and rotation.
    #[test]
    fn an_image_is_turned_the_way_its_file_says() {
        let dir = tempfile::tempdir().unwrap();
        // A JPEG's first segments: an Exif APP1 whose one-entry directory says 6, a quarter turn
        // clockwise.
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1, 0, 34];
        jpeg.extend(b"Exif\0\0II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0");
        jpeg.extend([0xFF, 0xDA, 0, 2]);
        let path = dir.path().join("turned.jpg");
        std::fs::write(&path, jpeg).unwrap();
        let path = path.to_string_lossy().into_owned();
        // 3 × 2: a b c over d e f.
        let rgba: Vec<u8> = b"abcdef".iter().flat_map(|&c| [c, 0, 0, 255]).collect();
        let turned = upright_image(rgba.clone(), 3, 2, path.clone());
        let firsts: Vec<u8> = turned.rgba.chunks(4).map(|p| p[0]).collect();
        assert_eq!(
            (turned.width, turned.height, &firsts[..]),
            (2, 3, &b"daebfc"[..])
        );

        let missing = dir.path().join("none.jpg").to_string_lossy().into_owned();
        assert_eq!(upright_image(rgba.clone(), 3, 2, missing).rgba, rgba);
        assert_eq!(
            upright_image(rgba.clone(), 2, 2, path).rgba,
            rgba,
            "short buffer"
        );
    }

    #[test]
    fn an_svg_gets_the_filter_unless_plain() {
        let svg =
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><path d=\"M0 0H9\"/></svg>".to_string();
        assert_eq!(
            recolour_svg(svg.clone(), Theme::Plain, true),
            Some(svg.clone())
        );
        let out = recolour_svg(svg.clone(), dark(), false).unwrap();
        assert!(out.contains("<feColorMatrix type=\"matrix\""), "{out}");
        assert!(!out.contains("<rect"), "{out}");
        assert!(recolour_svg(svg, dark(), true).unwrap().contains("<rect"));
        assert_eq!(recolour_svg("not a drawing".into(), dark(), false), None);
    }
}
