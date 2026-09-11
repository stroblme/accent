//! Standard base64 (RFC 4648, `+/`, `=` padding), for embedded images and the compressed page
//! form. A few lines are cheaper than a dependency for the two directions we need.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `None` for a character outside the alphabet. Whitespace is skipped and padding is optional,
/// because both turn up in files people have edited by hand.
pub fn decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = acc << 6 | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn round_trips_every_padding_length() {
        for s in ["", "f", "fo", "foo", "foob", "fooba", "foobar"] {
            let encoded = super::encode(s.as_bytes());
            assert_eq!(super::decode(&encoded).unwrap(), s.as_bytes(), "{encoded}");
        }
        assert_eq!(super::encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(super::encode(b"fo"), "Zm8=");
        assert_eq!(super::decode("Zm8").unwrap(), b"fo", "padding is optional");
        assert_eq!(super::decode("Zm9v\nYmFy").unwrap(), b"foobar");
        assert!(super::decode("Zm9v*").is_none());
    }
}
