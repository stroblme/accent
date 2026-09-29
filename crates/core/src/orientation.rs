//! Which way up a camera's picture is to be seen, and turning its pixels that way.
//!
//! A camera stores the sensor's rows as they came and writes how to turn them into the EXIF
//! Orientation tag. GDK's decoders ignore the tag (GTK 4.22), and so does Android's
//! `BitmapFactory`, so both apps read it here and turn the decoded pixels themselves.

use std::io::{Read, Seek, SeekFrom};

/// The EXIF Orientation tag (1–8) of the JPEG, TIFF or WebP in `file`, or 1 — upright — when it
/// has none or is none of those. Read where the tag is, which in a TIFF may be anywhere in the
/// file, and in a WebP comes after the pixels.
pub fn read(file: &mut (impl Read + Seek)) -> u8 {
    exif(file)
        .and_then(|at| tagged(file, at))
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

/// Where the TIFF structure holding `file`'s EXIF starts: the file itself for a TIFF.
fn exif(file: &mut (impl Read + Seek)) -> Option<u64> {
    match bytes::<12>(file, 0)? {
        [b'I', b'I', 42, 0, ..] | [b'M', b'M', 0, 42, ..] => Some(0),
        [0xFF, 0xD8, ..] => jpeg(file),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P'] => webp(file),
        _ => None,
    }
}

/// The TIFF structure in a JPEG's Exif APP1 segment, which comes before the image data.
fn jpeg(file: &mut (impl Read + Seek)) -> Option<u64> {
    let mut at = 2;
    loop {
        let [0xFF, marker, hi, lo] = bytes(file, at)? else {
            return None;
        };
        // Start of scan: the image data follows, and no more segments.
        if marker == 0xDA {
            return None;
        }
        if marker == 0xE1 && &bytes(file, at + 4)? == b"Exif\0\0" {
            return Some(at + 10);
        }
        at += 2 + u64::from(u16::from_be_bytes([hi, lo]));
    }
}

/// The TIFF structure in a WebP's EXIF chunk, which an extended (VP8X) file flags in its first
/// chunk and keeps after the image data.
fn webp(file: &mut (impl Read + Seek)) -> Option<u64> {
    let [b'V', b'P', b'8', b'X', _, _, _, _, flags] = bytes(file, 12)? else {
        return None;
    };
    if flags & 0x08 == 0 {
        return None;
    }
    let mut at = 12;
    loop {
        let [a, b, c, d, size @ ..] = bytes::<8>(file, at)?;
        if &[a, b, c, d] == b"EXIF" {
            // Some writers keep the JPEG segment's `Exif\0\0` in front of it.
            let prefixed = bytes(file, at + 8) == Some(*b"Exif\0\0");
            return Some(at + 8 + if prefixed { 6 } else { 0 });
        }
        // A chunk is padded to an even length.
        let size = u64::from(u32::from_le_bytes(size));
        at += 8 + size + (size & 1);
    }
}

/// The Orientation tag's value in the first directory of the TIFF structure at `at`, the
/// directory being wherever its offset points.
fn tagged(file: &mut (impl Read + Seek), at: u64) -> Option<u8> {
    let [o1, o2, _, _, ifd @ ..] = bytes::<8>(file, at)?;
    let big = match &[o1, o2] {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let short = |b: [u8; 2]| match big {
        true => u16::from_be_bytes(b),
        false => u16::from_le_bytes(b),
    };
    let ifd = at
        + u64::from(match big {
            true => u32::from_be_bytes(ifd),
            false => u32::from_le_bytes(ifd),
        });
    let mut entries = vec![0; 12 * usize::from(short(bytes(file, ifd)?))];
    file.read_exact(&mut entries).ok()?;
    // Twelve bytes an entry: tag, type, count, then the value itself when it fits in four.
    entries
        .chunks(12)
        .find(|entry| short([entry[0], entry[1]]) == 0x0112)
        .and_then(|entry| u8::try_from(short([entry[8], entry[9]])).ok())
}

/// `N` bytes of `file` from `at`, or `None` past its end.
fn bytes<const N: usize>(file: &mut (impl Read + Seek), at: u64) -> Option<[u8; N]> {
    let mut out = [0; N];
    file.seek(SeekFrom::Start(at)).ok()?;
    file.read_exact(&mut out).ok()?;
    Some(out)
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

    /// `bytes` read as a file is.
    fn read_of(bytes: Vec<u8>) -> u8 {
        read(&mut std::io::Cursor::new(bytes))
    }

    #[test]
    fn the_tag_is_read_from_a_jpegs_exif_in_either_byte_order() {
        for big in [false, true] {
            for tag in 1..=8 {
                assert_eq!(read_of(jpeg(tag, big)), tag as u8, "tag {tag} big={big}");
            }
        }
        // No Exif segment, or no JPEG at all: upright.
        assert_eq!(
            read_of([&[0xFF, 0xD8, 0xFF, 0xDA, 0, 2][..], &[0; 8]].concat()),
            1
        );
        assert_eq!(read_of(b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec()), 1);
    }

    /// A TIFF's first directory may come after its pixels, as libtiff writes one: far past any
    /// head of the file a reader would take.
    #[test]
    fn the_tag_is_read_from_a_tiffs_directory_wherever_it_is() {
        for big in [false, true] {
            for tag in 1..=8 {
                let file = tiff(tag, big, 100_000);
                assert_eq!(read_of(file), tag as u8, "tag {tag} big={big}");
            }
        }
    }

    /// A WebP keeps its EXIF in a chunk after the image data, which only an extended file flags.
    #[test]
    fn the_tag_is_read_from_a_webps_exif_chunk() {
        for prefix in [&b""[..], b"Exif\0\0"] {
            let exif = [prefix, &tiff(6, false, 0)].concat();
            assert_eq!(read_of(webp(0x08, &exif)), 6, "prefix {prefix:?}");
        }
        assert_eq!(read_of(webp(0, &tiff(6, false, 0))), 1, "not flagged");
    }

    /// A TIFF structure whose first directory holds an image width and then the orientation,
    /// after `pixels` bytes standing in for the image data.
    fn tiff(tag: u32, big: bool, pixels: usize) -> Vec<u8> {
        // A field of `len` bytes in the TIFF's byte order.
        let field = |v: u32, len: usize| match big {
            true => v.to_be_bytes()[4 - len..].to_vec(),
            false => v.to_le_bytes()[..len].to_vec(),
        };
        [
            (if big { b"MM" } else { b"II" }).to_vec(),
            field(42, 2),
            field(8 + pixels as u32, 4),
            vec![0; pixels],
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
        .concat()
    }

    /// A JPEG's first segments: a JFIF APP0, then an Exif APP1 holding [`tiff`].
    fn jpeg(tag: u32, big: bool) -> Vec<u8> {
        let tiff = tiff(tag, big, 0);
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16];
        out.extend(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        out.extend([0xFF, 0xE1]);
        out.extend((2 + 6 + tiff.len() as u16).to_be_bytes());
        out.extend(b"Exif\0\0");
        out.extend(tiff);
        out.extend([0xFF, 0xDA, 0, 2]);
        out
    }

    /// A WebP: a VP8X chunk with `flags`, an image chunk of odd length, so padded, and an EXIF
    /// chunk holding `exif`.
    fn webp(flags: u8, exif: &[u8]) -> Vec<u8> {
        let chunk = |name: &[u8], data: &[u8]| {
            let pad = vec![0; data.len() % 2];
            [name, &(data.len() as u32).to_le_bytes(), data, &pad].concat()
        };
        let body = [
            b"WEBP".to_vec(),
            chunk(b"VP8X", &[flags, 0, 0, 0, 1, 0, 0, 1, 0, 0]),
            chunk(b"VP8L", &[0x2f, 0, 0, 0, 0]),
            chunk(b"EXIF", exif),
        ]
        .concat();
        [
            b"RIFF".to_vec(),
            (body.len() as u32).to_le_bytes().to_vec(),
            body,
        ]
        .concat()
    }
}
