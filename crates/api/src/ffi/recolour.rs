//! Images recoloured like a PDF page, the test for which ones want it, and turning them upright.
//!
//! The rules are the core's ([`accent_core::recolour`], [`accent_core::orientation`]): Android
//! decodes with its own decoder and hands the pixels over as straight-alpha RGBA8, in the same
//! [`Theme`] a PDF page is rendered in, so a figure and a page in one vault land on the same paper.

use accent_core::orientation;
use accent_core::pdf;
use accent_core::recolour;

use crate::ffi::convert::{Theme, Tile};

/// Whether `width` × `height` straight-alpha RGBA8 pixels read as a document — a scan, plot,
/// diagram or screenshot of text — rather than a photo, and so want recolouring. A buffer that
/// does not match its size, or an image above 64 MP, is never one. See [`recolour::classify`].
#[uniffi::export]
pub fn looks_like_document(rgba: Vec<u8>, width: u32, height: u32) -> bool {
    recolour::classify(&rgba, width, height).document
}

/// Straight-alpha RGBA8 pixels moved onto the theme's paper and ink, alpha untouched unless
/// `fill` lays them on that paper, for a page that is not the screen's own
/// ([`recolour::onto_white`]); under [`Theme::Plain`] they come back as they went in.
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

/// Straight-alpha RGBA8 pixels `width` × `height`, decoded from the image file at `path`, turned
/// the way its EXIF Orientation tag says, which `BitmapFactory` ignores; they come back as they
/// went in when the file has no tag, or when they do not match their size.
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

/// An SVG's text with the same recolouring as a filter, so it stays vector, on the paper when
/// `fill` asks for it ([`recolour::recolour_svg_on_paper`]); under [`Theme::Plain`] it comes back
/// as it went in. `None` when there is no root `<svg>` element to hang the filter on.
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

    /// The reading and the turning are the core's and tested there; what is tested here is
    /// that a file's tag reaches the pixels, and that they come back whole without one.
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
