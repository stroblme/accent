//! Baseline JPEG files without an encoder: flat 8×8 blocks, whose DC coefficients are all a
//! scan has to code.

use super::Gen;

/// A JPEG marker segment: 0xFF, the marker, and a big-endian length that counts itself.
fn jpg_seg(out: &mut Vec<u8>, marker: u8, body: &[u8]) {
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(body);
}

/// JPEG magnitude coding: how many bits a coefficient needs, and those bits — a negative
/// value one less than itself, truncated, which is what the format asks for.
fn jpg_magnitude(v: i32) -> (u32, u32) {
    let cat = 32 - v.unsigned_abs().leading_zeros();
    let bits = if v < 0 { v - 1 } else { v } as u32 & ((1u32 << cat) - 1);
    (cat, bits)
}

/// MSB-first bit sink for a JPEG scan, with the stuffed zero every 0xFF byte needs.
#[derive(Default)]
struct Bits {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Bits {
    fn put(&mut self, code: u32, len: u32) {
        self.acc = (self.acc << len) | code;
        self.n += len;
        while self.n >= 8 {
            self.n -= 8;
            let b = (self.acc >> self.n) as u8;
            self.out.push(b);
            if b == 0xFF {
                self.out.push(0);
            }
        }
    }
    /// Pad the last byte with 1 bits, as the spec asks.
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            let pad = 8 - self.n;
            self.put((1 << pad) - 1, pad);
        }
        self.out
    }
}

impl Gen {
    /// Baseline JPEG of roughly `n` bytes with a grey diagonal ramp of flat 8×8 blocks. Fixed
    /// tables and DC-only blocks avoid a DCT or compressor; an application segment holds noise
    /// to meet the target size without a large image.
    pub(super) fn jpg(&mut self, n: usize) -> Vec<u8> {
        let side = (n / 3).isqrt().clamp(64, 512) & !7; // whole 8x8 blocks
        let blocks = side / 8;
        let mut bits = Bits::default();
        let mut prev = 0i32;
        for b in 0..blocks * blocks {
            let level = (b / blocks + b % blocks) * 255 / (2 * blocks - 2);
            let dc = 8 * (level as i32 - 128);
            let (cat, mag) = jpg_magnitude(dc - prev);
            prev = dc;
            bits.put(cat, 5); // the DC table is built so that code == category
            bits.put(mag, cat);
            bits.put(0, 2); // AC: end of block, and there is nothing else to say
        }
        let scan = bits.finish();

        let mut v = Vec::with_capacity(n + 256);
        v.extend_from_slice(&[0xFF, 0xD8]); // SOI
        jpg_seg(&mut v, 0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"); // APP0
        let mut dqt = vec![0u8]; // 8-bit precision, table 0
        dqt.extend_from_slice(&[1u8; 64]); // no quantisation at all, so DC survives exactly
        jpg_seg(&mut v, 0xDB, &dqt);
        let s = (side as u16).to_be_bytes();
        // SOF0: 8-bit, square, one greyscale component at 1x1 sampling, quantiser 0.
        jpg_seg(&mut v, 0xC0, &[8, s[0], s[1], s[0], s[1], 1, 1, 0x11, 0]);
        // DHT, DC table 0: all 16 categories five bits long, so the code *is* the category.
        // Five and not four, because a code of all ones is reserved: 16 four-bit codes would
        // use it up and libjpeg rejects the table.
        let mut dc = vec![0x00, 0, 0, 0, 0, 16];
        dc.extend_from_slice(&[0u8; 11]);
        dc.extend(0u8..16);
        jpg_seg(&mut v, 0xC4, &dc);
        // DHT, AC table 0: end-of-block and the run-of-16 the spec pairs with it, two bits
        // each. Only end-of-block is ever emitted, and it is the code 00.
        let mut ac = vec![0x10, 0, 2];
        ac.extend_from_slice(&[0u8; 14]);
        ac.extend_from_slice(&[0x00, 0xF0]);
        jpg_seg(&mut v, 0xC4, &ac);
        // Padding, before the scan so the geometry is never hidden behind noise. APP9 rather
        // than a comment segment: `file` prints comments, and nobody wants 4 MiB of them.
        let mut left = n.saturating_sub(v.len() + scan.len() + 12); // + SOS and EOI
        while left > 4 {
            let take = (left - 4).min(0xFFFD);
            let mut app = Vec::with_capacity(take);
            self.payload(take, &mut app);
            jpg_seg(&mut v, 0xE9, &app);
            left -= take + 4;
        }
        jpg_seg(&mut v, 0xDA, &[1, 1, 0x00, 0, 63, 0]); // SOS: one component, both tables 0
        v.extend_from_slice(&scan);
        v.extend_from_slice(&[0xFF, 0xD9]); // EOI
        v
    }
}
