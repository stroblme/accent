//! Objects back into PDF syntax (ISO 32000-1 §7.3), as the reader reads them, and a document of
//! one blank page.

use crate::{Dict, Object, Ref};

/// A real as PDF spells one: no exponent, at most six decimals, no trailing zeros.
pub(crate) fn real(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    match s {
        "-0" | "" => "0".into(),
        s => s.to_string(),
    }
}

/// `object` appended to `out`.
pub(crate) fn object(out: &mut Vec<u8>, object: &Object) {
    match object {
        Object::Null => out.extend(b"null"),
        Object::Bool(b) => out.extend(if *b { b"true".as_slice() } else { b"false" }),
        Object::Int(i) => out.extend(i.to_string().bytes()),
        Object::Real(r) => out.extend(real(*r).bytes()),
        Object::Name(n) => name(out, n),
        Object::String(s) => string(out, s),
        Object::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                self::object(out, item);
            }
            out.push(b']');
        }
        Object::Dict(d) => dict(out, d),
        Object::Stream(s) => {
            // `/Length` direct and true, wherever the dictionary had it.
            let mut d = s.dict.clone();
            d.set(b"Length", Object::Int(s.raw.len() as i64));
            dict(out, &d);
            out.extend(b"\nstream\n");
            out.extend(&s.raw);
            out.extend(b"\nendstream");
        }
        Object::Ref(r) => out.extend(format!("{} {} R", r.num, r.generation).bytes()),
    }
}

/// `n g obj … endobj` for `r`.
pub(crate) fn indirect(out: &mut Vec<u8>, r: Ref, value: &Object) {
    out.extend(format!("{} {} obj\n", r.num, r.generation).bytes());
    object(out, value);
    out.extend(b"\nendobj\n");
}

fn dict(out: &mut Vec<u8>, d: &Dict) {
    out.extend(b"<<");
    for (key, value) in &d.0 {
        name(out, key);
        out.push(b' ');
        object(out, value);
    }
    out.extend(b">>");
}

/// `/name`, every byte outside the regular printable ones written `#xx`.
fn name(out: &mut Vec<u8>, n: &[u8]) {
    out.push(b'/');
    for &b in n {
        match b {
            b'!'..=b'~' if !b"#()<>[]{}/%".contains(&b) => out.push(b),
            _ => out.extend(format!("#{b:02X}").bytes()),
        }
    }
}

/// A literal `(…)` string, or a hex one for bytes that are not text.
fn string(out: &mut Vec<u8>, s: &[u8]) {
    if s.iter()
        .any(|&b| !(b' '..=b'~').contains(&b) && !b"\n\r\t".contains(&b))
    {
        out.push(b'<');
        for b in s {
            out.extend(format!("{b:02X}").bytes());
        }
        out.push(b'>');
        return;
    }
    out.push(b'(');
    for &b in s {
        match b {
            b'(' | b')' | b'\\' => out.extend([b'\\', b]),
            // A raw CR would be read back as a line end, which is LF.
            b'\r' => out.extend(b"\\r"),
            b => out.push(b),
        }
    }
    out.push(b')');
}

/// An xref table of `rows`, each object's number, generation and offset, in subsections of
/// consecutive numbers; with `free_head`, object 0 as a new file's table starts.
pub(crate) fn table(out: &mut Vec<u8>, rows: &[(Ref, u64)], free_head: bool) {
    out.extend(b"xref\n");
    if free_head {
        out.extend(b"0 1\n0000000000 65535 f \n");
    }
    for run in rows.chunk_by(|a, b| b.0.num == a.0.num + 1) {
        out.extend(format!("{} {}\n", run[0].0.num, run.len()).bytes());
        for (r, offset) in run {
            out.extend(format!("{offset:010} {:05} n \n", r.generation).bytes());
        }
    }
}

/// A complete file of one blank page `size` points across: what a sketch or a drawing starts on.
pub fn new_document((width, height): (f32, f32)) -> Vec<u8> {
    let objects = [
        "<</Type/Catalog/Pages 2 0 R>>".to_string(),
        "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
        format!(
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 {} {}]/Resources<<>>>>",
            real(f64::from(width)),
            real(f64::from(height))
        ),
    ];
    // The binary comment tells a transfer program the file is not text.
    let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut rows = Vec::new();
    for (i, text) in objects.iter().enumerate() {
        let r = Ref {
            num: i as u32 + 1,
            generation: 0,
        };
        rows.push((r, out.len() as u64));
        out.extend(format!("{} 0 obj\n{text}\nendobj\n", r.num).bytes());
    }
    let xref = out.len();
    table(&mut out, &rows, true);
    let tail = format!("trailer\n<</Size 4/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n");
    out.extend(tail.bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::Lexer;

    #[test]
    fn what_is_written_reads_back_the_same() {
        let objects = [
            Object::Real(-0.000_000_1),
            Object::Real(1234.5),
            Object::Name(b"A B#(c)/".to_vec()),
            Object::String(b"a(b)\\c\r\nd".to_vec()),
            Object::String(vec![0, 0xff, b'(']),
            Object::Array(vec![Object::Null, Object::Bool(false), Object::Int(-3)]),
            Object::Dict(Dict(vec![
                (
                    b"K".to_vec(),
                    Object::Ref(Ref {
                        num: 12,
                        generation: 1,
                    }),
                ),
                (b"L".to_vec(), Object::Array(vec![])),
            ])),
        ];
        for o in objects {
            let mut out = Vec::new();
            object(&mut out, &o);
            let back = Lexer::new(&out, 0, true).object().unwrap();
            let want = match o {
                Object::Real(r) if r.abs() < 1e-6 => Object::Int(0),
                o => o,
            };
            assert_eq!(back, want, "{}", String::from_utf8_lossy(&out));
        }
        assert_eq!(real(1.0 / 3.0), "0.333333");
        assert_eq!(real(-2.5), "-2.5");
    }
}
