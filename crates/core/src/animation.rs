//! Whether an image file moves. GDK and `BitmapFactory` decode one still of an animated GIF, WebP
//! or PNG, so both apps leave an animation as its file is rather than recolour that still.

use std::io::{self, BufRead, BufReader, Read};

/// Whether the GIF, WebP or PNG in `file` holds more than one frame: a WebP flags it in its VP8X
/// chunk, a GIF has a second image after the first one's data, and a PNG an `acTL` chunk before
/// its image data. False for any other file, and for one that ends first. Read from the start, as
/// far as what says so at most.
pub fn animated(file: impl Read) -> bool {
    let mut file = BufReader::new(file);
    match bytes::<8>(&mut file) {
        Some([b'G', b'I', b'F', b'8', b'7' | b'9', b'a', ..]) => gif(&mut file),
        Some([b'R', b'I', b'F', b'F', ..]) => webp(&mut file),
        Some(head) if head == *b"\x89PNG\r\n\x1a\n" => png(&mut file),
        _ => None,
    }
    .unwrap_or(false)
}

/// Whether a WebP, read past its RIFF header's size, flags an animation: an extended one starts
/// with a VP8X chunk, whose first byte holds the flags.
fn webp(file: &mut impl BufRead) -> Option<bool> {
    let head = bytes::<13>(file)?;
    Some(head.starts_with(b"WEBPVP8X") && head[12] & 0x02 != 0)
}

/// Whether a PNG, read past its signature, has an `acTL` chunk before the first `IDAT`, where the
/// APNG specification puts it; one after is not an animation to any decoder.
fn png(file: &mut impl BufRead) -> Option<bool> {
    loop {
        let [l0, l1, l2, l3, name @ ..] = bytes::<8>(file)?;
        match &name {
            b"acTL" => return Some(true),
            b"IDAT" => return Some(false),
            // The chunk's data, and the CRC after it.
            _ => skip(file, u64::from(u32::from_be_bytes([l0, l1, l2, l3])) + 4)?,
        }
    }
}

/// Whether a GIF, read past its signature and its screen's width, holds a second image. `None`
/// where the file ends first.
fn gif(file: &mut impl BufRead) -> Option<bool> {
    // The rest of the screen descriptor: the height, then whether a global colour table follows.
    let [_, _, packed, _, _] = bytes(file)?;
    skip(file, table(packed))?;
    let mut seen = false;
    loop {
        match bytes::<1>(file)? {
            [0x2C] if seen => return Some(true),
            [0x2C] => {
                seen = true;
                let [.., packed] = bytes::<9>(file)?;
                // The local colour table, and the LZW code size the image's data starts with.
                skip(file, table(packed) + 1)?;
            }
            // An extension's label: its data follows as an image's does.
            [0x21] => skip(file, 1)?,
            // The trailer, or no block at all.
            _ => return Some(false),
        }
        // Sub-blocks of up to 255 bytes, each after its length, until an empty one.
        loop {
            match bytes::<1>(file)? {
                [0] => break,
                [len] => skip(file, len.into())?,
            }
        }
    }
}

/// The length of the colour table a GIF descriptor's `packed` byte announces: none, or 2 to 256
/// colours of three bytes each.
fn table(packed: u8) -> u64 {
    match packed & 0x80 {
        0 => 0,
        _ => 3 << ((packed & 0x07) + 1),
    }
}

/// Read past `n` bytes of `file`, or `None` where it ends first.
fn skip(file: &mut impl BufRead, n: u64) -> Option<()> {
    let skipped = io::copy(&mut file.take(n), &mut io::sink()).ok()?;
    (skipped == n).then_some(())
}

/// The next `N` bytes of `file`, or `None` past its end.
fn bytes<const N: usize>(file: &mut impl Read) -> Option<[u8; N]> {
    let mut out = [0; N];
    file.read_exact(&mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2 × 2 GIF89a with a global colour table, looping, of `frames` images: each after a
    /// Graphic Control extension, the first with a local colour table, and data whose sub-block
    /// holds the bytes that open an image and an extension. A comment ends it.
    fn gif(frames: usize) -> Vec<u8> {
        let mut out = b"GIF89a\x02\x00\x02\x00\x80\x00\x00".to_vec();
        out.extend([0, 0, 0, 0x2C, 0x21, 0xFF]);
        out.extend(b"\x21\xFF\x0BNETSCAPE2.0\x03\x01\x00\x00\x00");
        for frame in 0..frames {
            out.extend(b"\x21\xF9\x04\x04\x0A\x00\x00\x00");
            match frame {
                0 => out.extend(b"\x2C\0\0\0\0\x02\0\x02\0\x80\x2C\x21\x3B\x21\x2C\x00"),
                _ => out.extend(b"\x2C\0\0\0\0\x02\0\x02\0\x00"),
            }
            out.extend(b"\x02\x04\x2C\x21\x3B\x2C\x00");
        }
        out.extend(b"\x21\xFE\x03abc\x00\x3B");
        out
    }

    /// A PNG whose text chunk holds the names of the chunks that matter, then an `acTL` where
    /// it is `animated`, the image data, and an `acTL` after it, where none counts.
    fn png(animated: bool) -> Vec<u8> {
        let chunk = |name: &[u8], data: &[u8]| {
            [&(data.len() as u32).to_be_bytes()[..], name, data, &[0; 4]].concat()
        };
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        out.extend(chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]));
        out.extend(chunk(b"tEXt", b"acTLIDAT"));
        if animated {
            out.extend(chunk(b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]));
        }
        out.extend(chunk(b"IDAT", &[0x78, 0x9C, 0x63, 0, 0, 0, 2, 0, 1]));
        out.extend(chunk(b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]));
        out.extend(chunk(b"IEND", b""));
        out
    }

    /// A WebP of a VP8X chunk with `flags`, then an image chunk.
    fn webp(flags: u8) -> Vec<u8> {
        let mut body = b"WEBPVP8X\x0A\0\0\0".to_vec();
        body.extend([flags, 0, 0, 0, 1, 0, 0, 1, 0, 0]);
        body.extend(b"VP8L\x05\0\0\0\x2F\0\0\0\0\0");
        [&b"RIFF"[..], &(body.len() as u32).to_le_bytes(), &body].concat()
    }

    #[test]
    fn a_gif_of_two_images_is_an_animation_and_one_of_one_is_not() {
        assert!(animated(&gif(2)[..]));
        assert!(animated(&gif(3)[..]));
        assert!(!animated(&gif(1)[..]), "a still, whatever its data holds");
        assert!(!animated(&gif(0)[..]));
    }

    #[test]
    fn a_webp_is_an_animation_where_its_vp8x_chunk_says_so() {
        assert!(animated(&webp(0x02)[..]));
        assert!(animated(&webp(0x0A)[..]), "and has EXIF");
        assert!(!animated(&webp(0x08)[..]), "EXIF alone");
        assert!(!animated(
            &b"RIFF\x0E\0\0\0WEBPVP8L\x05\0\0\0\x2F\0\0\0\0\0"[..]
        ));
    }

    #[test]
    fn a_png_is_an_animation_where_an_actl_chunk_comes_before_its_image_data() {
        assert!(animated(&png(true)[..]));
        assert!(!animated(&png(false)[..]));
    }

    /// Every head of a file reads without a panic, and as an animation only once it reaches the
    /// second image, the flag or the chunk.
    #[test]
    fn a_cut_file_is_an_animation_only_past_what_says_so() {
        let file = gif(2);
        // After the first image, the second's Graphic Control extension: eight bytes, as the
        // comment and trailer a GIF of one ends with are.
        let second = gif(1).len();
        assert_eq!(file[second], 0x2C);
        for n in 0..=file.len() {
            assert_eq!(animated(&file[..n]), n > second, "{n} bytes");
        }
        let file = webp(0x02);
        for n in 0..=file.len() {
            assert_eq!(animated(&file[..n]), n > 20, "{n} bytes");
        }
        let file = png(true);
        // The signature, the header and text chunks, then the `acTL`'s length and name.
        let after = 8 + 25 + 20 + 8;
        assert_eq!(file[after - 4..after], *b"acTL");
        for n in 0..=file.len() {
            assert_eq!(animated(&file[..n]), n >= after, "{n} bytes");
        }
    }

    #[test]
    fn anything_else_is_a_still() {
        assert!(!animated(&b""[..]));
        assert!(!animated(&b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR"[..]));
        assert!(!animated(&b"GIF89a"[..]));
    }
}
