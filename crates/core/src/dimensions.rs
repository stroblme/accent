//! The pixel size a raster's header declares, read before it is decoded, so the desktop app can
//! refuse an image too large to hold before a decoder allocates it.

use std::io::{Read, Seek, SeekFrom};

/// The width and height the PNG, JPEG, GIF, WebP or TIFF in `file` declares, as stored (before
/// any EXIF turn), or `None` for any other file and one cut short of its header.
pub fn read(file: &mut (impl Read + Seek)) -> Option<(u32, u32)> {
    match bytes::<4>(file, 0)? {
        [0x89, b'P', b'N', b'G'] => match bytes::<12>(file, 12)? {
            [b'I', b'H', b'D', b'R', size @ ..] => {
                let [w0, w1, w2, w3, h0, h1, h2, h3] = size;
                Some((
                    u32::from_be_bytes([w0, w1, w2, w3]),
                    u32::from_be_bytes([h0, h1, h2, h3]),
                ))
            }
            _ => None,
        },
        [0xFF, 0xD8, ..] => jpeg(file),
        // The logical screen, which every frame is drawn onto.
        [b'G', b'I', b'F', b'8'] => {
            let [w0, w1, h0, h1] = bytes(file, 6)?;
            let le = |b: [u8; 2]| u32::from(u16::from_le_bytes(b));
            Some((le([w0, w1]), le([h0, h1])))
        }
        [b'R', b'I', b'F', b'F'] if bytes(file, 8) == Some(*b"WEBP") => webp(file),
        [b'I', b'I', 42, 0] => tiff(file, false),
        [b'M', b'M', 0, 42] => tiff(file, true),
        _ => None,
    }
}

/// A JPEG's frame header (SOF0 to SOF15), the segment before its image data that says its size.
fn jpeg(file: &mut (impl Read + Seek)) -> Option<(u32, u32)> {
    let mut at = 2;
    loop {
        let [0xFF, marker, hi, lo] = bytes(file, at)? else {
            return None;
        };
        // DHT, JPG and DAC share the frame headers' range.
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let [_precision, h0, h1, w0, w1] = bytes(file, at + 4)?;
            let be = |b: [u8; 2]| u32::from(u16::from_be_bytes(b));
            return Some((be([w0, w1]), be([h0, h1])));
        }
        // Start of scan: image data, and no frame header before it.
        if marker == 0xDA {
            return None;
        }
        at += 2 + u64::from(u16::from_be_bytes([hi, lo]));
    }
}

/// A WebP's first chunk: a lossy or a lossless image's own header, or an extended file's canvas.
fn webp(file: &mut (impl Read + Seek)) -> Option<(u32, u32)> {
    match &bytes::<4>(file, 12)? {
        // After the frame tag and the start code, 14 bits each, two bits of scale above them.
        b"VP8 " => {
            let [.., w0, w1, h0, h1] = bytes::<10>(file, 20)?;
            let side = |b: [u8; 2]| u32::from(u16::from_le_bytes(b) & 0x3FFF);
            Some((side([w0, w1]), side([h0, h1])))
        }
        // After the signature byte, 14 bits each of width and height less one.
        b"VP8L" => {
            let [_, b @ ..] = bytes::<5>(file, 20)?;
            let bits = u32::from_le_bytes(b);
            Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
        }
        // After the flags, 24 bits each of the canvas's width and height less one.
        b"VP8X" => {
            let [.., w0, w1, w2, h0, h1, h2] = bytes::<10>(file, 20)?;
            Some((
                u32::from_le_bytes([w0, w1, w2, 0]) + 1,
                u32::from_le_bytes([h0, h1, h2, 0]) + 1,
            ))
        }
        _ => None,
    }
}

/// A TIFF's ImageWidth and ImageLength, each a SHORT or a LONG in its first directory.
fn tiff(file: &mut (impl Read + Seek), big: bool) -> Option<(u32, u32)> {
    let short = |b: [u8; 2]| match big {
        true => u16::from_be_bytes(b),
        false => u16::from_le_bytes(b),
    };
    let long = |b: [u8; 4]| match big {
        true => u32::from_be_bytes(b),
        false => u32::from_le_bytes(b),
    };
    let ifd = u64::from(long(bytes(file, 4)?));
    let mut entries = vec![0; 12 * usize::from(short(bytes(file, ifd)?))];
    file.read_exact(&mut entries).ok()?;
    // Twelve bytes an entry: tag, type, count, then the value itself when it fits in four.
    let value = |tag: u16| {
        let e = entries.chunks(12).find(|e| short([e[0], e[1]]) == tag)?;
        match short([e[2], e[3]]) {
            3 => Some(u32::from(short([e[8], e[9]]))),
            4 => Some(long([e[8], e[9], e[10], e[11]])),
            _ => None,
        }
    };
    Some((value(256)?, value(257)?))
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

    /// `bytes` read as a file is.
    fn read_of(bytes: &[u8]) -> Option<(u32, u32)> {
        read(&mut std::io::Cursor::new(bytes))
    }

    /// A WebP of one chunk, `name` holding `data`.
    fn webp(name: &[u8], data: &[u8]) -> Vec<u8> {
        let body = [b"WEBP", name, &(data.len() as u32).to_le_bytes(), data].concat();
        [&b"RIFF"[..], &(body.len() as u32).to_le_bytes(), &body].concat()
    }

    /// Each format's header, cut down to what declares the size, says 3000 × 2000.
    #[test]
    fn each_format_declares_its_size_in_its_header() {
        let png = [
            &b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR"[..],
            &3000u32.to_be_bytes(),
            &2000u32.to_be_bytes(),
        ]
        .concat();
        // A JFIF segment, then a progressive frame header.
        let jpeg = [
            &b"\xFF\xD8\xFF\xE0\0\x10JFIF\0\x01\x01\0\0\x01\0\x01\0\0"[..],
            b"\xFF\xC2\0\x11\x08",
            &2000u16.to_be_bytes(),
            &3000u16.to_be_bytes(),
        ]
        .concat();
        let gif = [
            &b"GIF89a"[..],
            &3000u16.to_le_bytes(),
            &2000u16.to_le_bytes(),
        ]
        .concat();
        let lossy = webp(
            b"VP8 ",
            &[
                &[0, 0, 0, 0x9D, 0x01, 0x2A][..],
                &3000u16.to_le_bytes(),
                &2000u16.to_le_bytes(),
            ]
            .concat(),
        );
        let bits = (3000 - 1) | ((2000 - 1) << 14);
        let lossless = webp(b"VP8L", &[&[0x2F][..], &u32::to_le_bytes(bits)].concat());
        let extended = webp(
            b"VP8X",
            &[
                &[0; 4][..],
                &(3000 - 1u32).to_le_bytes()[..3],
                &(2000 - 1u32).to_le_bytes()[..3],
            ]
            .concat(),
        );
        // Little-endian, its width a SHORT and its height a LONG, after a tag of its own.
        let entry = |tag: u16, kind: u16, value: u32| {
            [
                &tag.to_le_bytes()[..],
                &kind.to_le_bytes(),
                &1u32.to_le_bytes(),
                &value.to_le_bytes(),
            ]
            .concat()
        };
        let tiff = [
            &b"II*\0\x08\0\0\0\x03\0"[..],
            &entry(254, 4, 0),
            &entry(256, 3, 3000),
            &entry(257, 4, 2000),
        ]
        .concat();
        for (name, file) in [
            ("png", png),
            ("jpeg", jpeg),
            ("gif", gif),
            ("lossy webp", lossy),
            ("lossless webp", lossless),
            ("extended webp", extended),
            ("tiff", tiff),
        ] {
            assert_eq!(read_of(&file), Some((3000, 2000)), "{name}");
            // Cut short of its size, it declares none, and reading it does not panic.
            for n in 0..file.len() {
                assert_eq!(read_of(&file[..n]), None, "{name} cut to {n} bytes");
            }
        }
    }

    #[test]
    fn anything_else_declares_nothing() {
        assert_eq!(read_of(b""), None);
        assert_eq!(read_of(b"<svg width=\"60000\" height=\"60000\"/>"), None);
        // A JPEG whose image data starts before any frame header.
        assert_eq!(read_of(b"\xFF\xD8\xFF\xDA\0\x02\0\0\0\0\0\0"), None);
    }
}
