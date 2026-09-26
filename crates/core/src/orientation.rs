//! Which way up a camera's JPEG is to be seen, and turning its pixels that way.
//!
//! A camera stores the sensor's rows as they came and writes how to turn them into the EXIF
//! Orientation tag. GDK's decoders ignore the tag (GTK 4.22), so the image tab reads it here and
//! turns the decoded pixels itself.

/// The EXIF Orientation tag (1–8) of the JPEG whose first bytes are `head`, or 1 — upright — when
/// it has none or is not a JPEG. The tag is in the first segments, so the file's first 64 KiB
/// hold it.
pub fn read(head: &[u8]) -> u8 {
    exif(head)
        .and_then(tagged)
        .filter(|tag| (1..=8).contains(tag))
        .unwrap_or(1)
}

/// Straight RGBA8 pixels `width` × `height` turned as `tag` asks, with their new width and height.
pub fn apply(data: &[u8], width: u32, height: u32, tag: u8) -> (Vec<u8>, u32, u32) {
    let (w, h) = (width as usize, height as usize);
    // 5 to 8 turn the image a quarter, so its rows become columns.
    let (out_w, out_h) = if tag >= 5 { (h, w) } else { (w, h) };
    let mut out = Vec::with_capacity(data.len());
    for y in 0..out_h {
        for x in 0..out_w {
            let (sx, sy) = match tag {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (y, h - 1 - x),
                7 => (w - 1 - y, h - 1 - x),
                8 => (w - 1 - y, x),
                _ => (x, y),
            };
            let at = (sy * w + sx) * 4;
            out.extend_from_slice(&data[at..at + 4]);
        }
    }
    (out, out_w as u32, out_h as u32)
}

/// The TIFF structure in a JPEG's Exif APP1 segment, cut short where `jpeg` ends.
fn exif(jpeg: &[u8]) -> Option<&[u8]> {
    if !jpeg.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut at = 2;
    loop {
        let &[0xFF, marker, hi, lo] = jpeg.get(at..at + 4)? else {
            return None;
        };
        // Start of scan: the image data follows, and no more segments.
        if marker == 0xDA {
            return None;
        }
        let end = at + 2 + usize::from(u16::from_be_bytes([hi, lo]));
        let body = jpeg.get(at + 4..end.min(jpeg.len()))?;
        if marker == 0xE1 && body.starts_with(b"Exif\0\0") {
            return Some(&body[6..]);
        }
        at = end;
    }
}

/// The Orientation tag's value in a TIFF structure's first directory.
fn tagged(tiff: &[u8]) -> Option<u8> {
    let big = match tiff.get(..2)? {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let u16_at = |at: usize| {
        let b: [u8; 2] = tiff.get(at..at + 2)?.try_into().ok()?;
        Some(if big {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        })
    };
    let b: [u8; 4] = tiff.get(4..8)?.try_into().ok()?;
    let ifd = match big {
        true => u32::from_be_bytes(b),
        false => u32::from_le_bytes(b),
    } as usize;
    // Twelve bytes an entry: tag, type, count, then the value itself when it fits in four.
    (0..usize::from(u16_at(ifd)?))
        .map(|i| ifd + 2 + 12 * i)
        .find(|&entry| u16_at(entry) == Some(0x0112))
        .and_then(|entry| u8::try_from(u16_at(entry + 8)?).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each tag against the picture it names, drawn out by hand for
    ///
    /// ```text
    /// a b c
    /// d e f
    /// ```
    #[test]
    fn every_tag_turns_the_pixels_upright() {
        let src: Vec<u8> = b"abcdef".iter().flat_map(|&c| [c, 0, 0, 255]).collect();
        let want: [(u8, &[u8], u32); 8] = [
            (1, b"abcdef", 3),
            (2, b"cbafed", 3),
            (3, b"fedcba", 3),
            (4, b"defabc", 3),
            (5, b"adbecf", 2),
            (6, b"daebfc", 2),
            (7, b"fcebda", 2),
            (8, b"cfbead", 2),
        ];
        for (tag, grid, width) in want {
            let (out, w, h) = apply(&src, 3, 2, tag);
            let got: Vec<u8> = out.chunks(4).map(|p| p[0]).collect();
            assert_eq!(
                (got.as_slice(), w, h),
                (grid, width, 6 / width),
                "tag {tag}"
            );
        }
    }

    #[test]
    fn the_tag_is_read_from_a_jpegs_exif_in_either_byte_order() {
        for big in [false, true] {
            for tag in 1..=8 {
                assert_eq!(read(&jpeg(tag, big)), tag as u8, "tag {tag} big={big}");
            }
        }
        // No Exif segment, or no JPEG at all: upright.
        assert_eq!(read(&[0xFF, 0xD8, 0xFF, 0xDA, 0, 2]), 1);
        assert_eq!(read(b"\x89PNG\r\n\x1a\n"), 1);
    }

    /// A JPEG's first segments: a JFIF APP0, then an Exif APP1 whose first directory holds an
    /// image width and then the orientation.
    fn jpeg(tag: u32, big: bool) -> Vec<u8> {
        // A field of `len` bytes in the TIFF's byte order.
        let field = |v: u32, len: usize| match big {
            true => v.to_be_bytes()[4 - len..].to_vec(),
            false => v.to_le_bytes()[..len].to_vec(),
        };
        let tiff = [
            (if big { b"MM" } else { b"II" }).to_vec(),
            field(42, 2),
            field(8, 4),
            field(2, 2),
            // Tag, type (LONG, SHORT), count, value.
            [field(0x0100, 2), field(4, 2), field(1, 4), field(640, 4)].concat(),
            [
                field(0x0112, 2),
                field(3, 2),
                field(1, 4),
                field(tag, 2),
                vec![0, 0],
            ]
            .concat(),
            field(0, 4),
        ]
        .concat();
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16];
        out.extend(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        out.extend([0xFF, 0xE1]);
        out.extend((2 + 6 + tiff.len() as u16).to_be_bytes());
        out.extend(b"Exif\0\0");
        out.extend(tiff);
        out.extend([0xFF, 0xDA, 0, 2]);
        out
    }
}
