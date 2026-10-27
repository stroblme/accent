//! Stream filters as far as the objects read here need them (ISO 32000-1 §7.4.4): FlateDecode
//! with the PNG and TIFF predictors, which xref and object streams are written in. Images and
//! fonts are pdfium's to decode.

use miniz_oxide::inflate::{TINFLStatus, decompress_to_vec_zlib_with_limit};

use crate::{Error, Object, Result};

/// The most one stream may inflate to, against a decompression bomb.
const MAX: usize = 64 << 20;

/// `raw` through `filters`, a name or an array of them, each with its entry of `parms`.
pub(crate) fn decode(
    raw: &[u8],
    filters: Option<&Object>,
    parms: Option<&Object>,
) -> Result<Vec<u8>> {
    let filters = match filters {
        None | Some(Object::Null) => return Ok(raw.to_vec()),
        Some(Object::Name(n)) => vec![n.as_slice()],
        Some(Object::Array(a)) => a.iter().filter_map(Object::as_name).collect(),
        Some(_) => return Err(Error::Broken("/Filter is no name".into())),
    };
    let mut data = raw.to_vec();
    for (i, filter) in filters.into_iter().enumerate() {
        let parms = match parms {
            Some(Object::Array(a)) => a.get(i),
            p => p.filter(|_| i == 0),
        };
        data = match filter {
            b"FlateDecode" | b"Fl" => predict(inflate(&data)?, parms.and_then(Object::as_dict))?,
            other => {
                let name = String::from_utf8_lossy(other);
                return Err(Error::Unsupported(format!("the {name} filter")));
            }
        };
    }
    Ok(data)
}

/// zlib data inflated. Data that stops short of its checksum, as some writers leave it, is taken
/// for what it holds, as pdfium takes it.
fn inflate(data: &[u8]) -> Result<Vec<u8>> {
    match decompress_to_vec_zlib_with_limit(data, MAX) {
        Ok(out) => Ok(out),
        Err(e) if e.status == TINFLStatus::HasMoreOutput => {
            Err(Error::Unsupported("a stream past 64 MiB".into()))
        }
        Err(e)
            if matches!(
                e.status,
                TINFLStatus::Adler32Mismatch | TINFLStatus::FailedCannotMakeProgress
            ) && !e.output.is_empty() =>
        {
            Ok(e.output)
        }
        Err(e) => Err(Error::Broken(format!("Flate data: {e}"))),
    }
}

/// Undo `/Predictor`: 2 (TIFF) or 10–15 (PNG, each row tagged with its own).
fn predict(data: Vec<u8>, parms: Option<&crate::Dict>) -> Result<Vec<u8>> {
    let get = |key: &[u8], default: i64| {
        parms
            .and_then(|p| p.get(key))
            .and_then(Object::as_int)
            .unwrap_or(default)
    };
    let predictor = get(b"Predictor", 1);
    if predictor < 2 {
        return Ok(data);
    }
    let (colors, bits, columns) = (
        get(b"Colors", 1),
        get(b"BitsPerComponent", 8),
        get(b"Columns", 1),
    );
    if !(1..=32).contains(&colors)
        || ![1, 2, 4, 8, 16].contains(&bits)
        || !(1..=1 << 20).contains(&columns)
    {
        return Err(Error::Broken("predictor parameters".into()));
    }
    // Bytes a pixel and a row: what the filters step back by.
    let bpp = ((colors * bits + 7) / 8) as usize;
    let row = ((colors * bits * columns + 7) / 8) as usize;
    if predictor == 2 {
        if bits != 8 {
            return Err(Error::Unsupported(format!("TIFF predictor at {bits} bits")));
        }
        let mut data = data;
        for line in data.chunks_mut(row) {
            for i in bpp..line.len() {
                line[i] = line[i].wrapping_add(line[i - bpp]);
            }
        }
        return Ok(data);
    }
    let mut out = Vec::with_capacity(data.len());
    let mut prev = vec![0u8; row];
    for line in data.chunks(row + 1) {
        let (tag, line) = (line[0], &line[1..]);
        let mut cur = line.to_vec();
        for i in 0..cur.len() {
            let left = if i >= bpp { cur[i - bpp] } else { 0 };
            let (up, corner) = (prev[i], if i >= bpp { prev[i - bpp] } else { 0 });
            cur[i] = cur[i].wrapping_add(match tag {
                0 => 0,
                1 => left,
                2 => up,
                3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
                4 => paeth(left, up, corner),
                _ => return Err(Error::Broken(format!("PNG filter {tag}"))),
            });
        }
        out.extend_from_slice(&cur);
        prev[..cur.len()].copy_from_slice(&cur);
    }
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = (
        (p - i16::from(a)).abs(),
        (p - i16::from(b)).abs(),
        (p - i16::from(c)).abs(),
    );
    match (pa <= pb && pa <= pc, pb <= pc) {
        (true, _) => a,
        (false, true) => b,
        (false, false) => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Dict;

    #[test]
    fn png_rows_are_undone_by_their_own_filter() {
        // Two rows of three bytes: Up over a first row given as is, then Sub.
        let rows = [2u8, 1, 2, 3, 1, 5, 1, 1];
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&rows, 6);
        let parms = Object::Dict(Dict(vec![
            (b"Predictor".to_vec(), Object::Int(12)),
            (b"Columns".to_vec(), Object::Int(3)),
        ]));
        let flate = Object::Name(b"FlateDecode".to_vec());
        let out = decode(&z, Some(&flate), Some(&parms)).unwrap();
        assert_eq!(out, [1, 2, 3, 5, 6, 7]);
    }

    #[test]
    fn a_bomb_stops_at_the_cap() {
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&vec![0u8; MAX + 1], 9);
        assert!(matches!(inflate(&z), Err(Error::Unsupported(_))));
    }
}
