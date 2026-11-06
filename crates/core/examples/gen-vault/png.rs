//! PNG files without an encoder: stored deflate blocks hold any pixels as a valid file.

use super::Gen;

/// CRC-32 as PNG and zlib define it. The table is built at compile time because every
/// image byte written goes through it.
const CRC32: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 == 0 {
                c >> 1
            } else {
                0xEDB8_8320 ^ (c >> 1)
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

fn crc32(bytes: &[u8]) -> u32 {
    let mut c = u32::MAX;
    for &b in bytes {
        c = CRC32[((c ^ u32::from(b)) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

/// Adler-32 with zlib's own deferred modulo: 5552 bytes can never overflow the accumulators.
fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for run in bytes.chunks(5552) {
        for &x in run {
            a += u32::from(x);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

/// Length, type, data, CRC over type+data — appended in place, so a multi-MiB IDAT is
/// never copied to be checksummed.
fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// A zlib stream of stored (uncompressed) deflate blocks: a valid IDAT without a
/// compressor. The payload is noise, so nothing would have compressed anyway.
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let mut z = vec![0x78, 0x01]; // deflate, 32 KiB window, no preset dictionary
    let mut blocks = raw.chunks(0xFFFF).peekable();
    while let Some(b) = blocks.next() {
        z.push(u8::from(blocks.peek().is_none())); // BFINAL on the last block only
        z.extend_from_slice(&(b.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(b.len() as u16)).to_le_bytes());
        z.extend_from_slice(b);
    }
    z.extend_from_slice(&adler32(raw).to_be_bytes());
    z
}

/// An 8-bit PNG of `w` × `h` pixels, RGB with 3 `channels` and RGBA with 4, in one stored
/// deflate stream.
fn png_of(w: usize, h: usize, channels: usize, px: &[u8]) -> Vec<u8> {
    // One filter byte (0 = None) in front of every scanline.
    let mut raw = Vec::with_capacity(h * (1 + w * channels));
    for row in px.chunks(w * channels) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let colour_type = if channels == 4 { 6 } else { 2 }; // truecolour, with alpha or without
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, colour_type, 0, 0, 0]); // 8 bits, no interlace
    let mut v = Vec::with_capacity(raw.len() + 128);
    v.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    png_chunk(&mut v, b"IHDR", &ihdr);
    png_chunk(&mut v, b"IDAT", &zlib_stored(&raw));
    png_chunk(&mut v, b"IEND", b"");
    v
}

/// A figure image auto-theming should recolour: a white bar chart, black axes and bars, and
/// one line in matplotlib's blue.
pub(super) fn figure_png() -> Vec<u8> {
    let (w, h) = (320, 200);
    let mut px = vec![255u8; w * h * 3];
    let mut paint = |xs: std::ops::Range<usize>, ys: std::ops::Range<usize>, c: [u8; 3]| {
        for y in ys {
            for x in xs.clone() {
                px[(y * w + x) * 3..][..3].copy_from_slice(&c);
            }
        }
    };
    paint(30..32, 20..180, [0; 3]);
    paint(30..300, 178..180, [0; 3]);
    for (i, top) in [120, 80, 140, 60, 100].into_iter().enumerate() {
        paint(50 + i * 50..74 + i * 50, top..178, [0; 3]);
    }
    for x in 32..300 {
        let y = (100.0 + 50.0 * (x as f64 / 30.0).sin()) as usize;
        paint(x..x + 1, y..y + 3, [0x1f, 0x77, 0xb4]);
    }
    png_of(w, h, 3, &px)
}

/// Transparent line-art fixture with black strokes, a ring and a diagonal.
pub(super) fn lineart_png() -> Vec<u8> {
    let (w, h) = (200, 200);
    let mut px = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f64 - 100.0, y as f64 - 100.0);
            let ring = ((dx * dx + dy * dy).sqrt() - 80.0).abs() < 1.5;
            if ring || (dx - dy).abs() < 2.0 {
                px[(y * w + x) * 4 + 3] = 255;
            }
        }
    }
    png_of(w, h, 4, &px)
}

impl Gen {
    /// Decodable 8-bit RGB PNG of roughly `n` bytes. Random pixels and stored deflate keep file
    /// size proportional to pixel count for the vault size distribution.
    pub(super) fn png(&mut self, n: usize) -> Vec<u8> {
        let w = (n / 3).isqrt().max(1);
        let h = (n / (3 * w + 1)).max(1);
        let mut px = Vec::with_capacity(w * h * 3);
        self.payload(w * h * 3, &mut px);
        png_of(w, h, 3, &px)
    }
}
