//! PDF syntax (ISO 32000-1 §7.2–7.3) over bytes in memory: a window of the file, which the caller
//! widens when an object runs past its end, or a decoded stream, which is whole.

use crate::{Dict, Object, Ref};

/// Why parsing stopped short of an object.
#[derive(Debug, PartialEq)]
pub(crate) enum Stop {
    /// The window ended mid-object: read more of the file and parse again.
    End,
    /// Not PDF syntax, at this offset of the window.
    Bad(usize, &'static str),
}

pub(crate) type Lexed<T> = std::result::Result<T, Stop>;

/// How deep arrays and dictionaries may nest, against a file built to exhaust the stack.
const DEPTH: usize = 64;

fn is_white(b: u8) -> bool {
    matches!(b, 0 | b'\t' | b'\n' | 0x0c | b'\r' | b' ')
}

fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn is_regular(b: u8) -> bool {
    !is_white(b) && !is_delimiter(b)
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

pub(crate) struct Lexer<'a> {
    buf: &'a [u8],
    pub pos: usize,
    /// The buffer is all there is: its end ends a token rather than asking for more.
    complete: bool,
}

impl<'a> Lexer<'a> {
    pub fn new(buf: &'a [u8], pos: usize, complete: bool) -> Lexer<'a> {
        Lexer { buf, pos, complete }
    }

    fn end(&self) -> Stop {
        match self.complete {
            true => Stop::Bad(self.pos, "unexpected end"),
            false => Stop::End,
        }
    }

    fn peek(&self) -> Lexed<u8> {
        self.buf.get(self.pos).copied().ok_or_else(|| self.end())
    }

    /// Past whitespace and comments.
    pub fn skip_white(&mut self) {
        while let Some(&b) = self.buf.get(self.pos) {
            if b == b'%' {
                while self
                    .buf
                    .get(self.pos)
                    .is_some_and(|&b| b != b'\n' && b != b'\r')
                {
                    self.pos += 1;
                }
            } else if is_white(b) {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// The next run of regular characters: a number or a keyword. Empty before a delimiter.
    pub fn word(&mut self) -> Lexed<&'a [u8]> {
        self.skip_white();
        self.run()
    }

    /// The regular characters from here.
    fn run(&mut self) -> Lexed<&'a [u8]> {
        let start = self.pos;
        while self.buf.get(self.pos).is_some_and(|&b| is_regular(b)) {
            self.pos += 1;
        }
        if self.pos == self.buf.len() && !self.complete {
            return Err(Stop::End);
        }
        Ok(&self.buf[start..self.pos])
    }

    pub fn keyword(&mut self, k: &[u8]) -> Lexed<()> {
        let at = self.pos;
        match self.word()? == k {
            true => Ok(()),
            false => Err(Stop::Bad(at, "unexpected keyword")),
        }
    }

    /// A non-negative integer word, as object numbers and xref fields are.
    pub fn unsigned(&mut self) -> Lexed<u64> {
        let at = self.pos;
        let w = self.word()?;
        std::str::from_utf8(w)
            .ok()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse().ok())
            .ok_or(Stop::Bad(at, "expected a number"))
    }

    /// One object, an `N G R` reference included.
    pub fn object(&mut self) -> Lexed<Object> {
        self.object_in(0)
    }

    fn object_in(&mut self, depth: usize) -> Lexed<Object> {
        if depth > DEPTH {
            return Err(Stop::Bad(self.pos, "nested too deep"));
        }
        self.skip_white();
        let at = self.pos;
        match self.peek()? {
            b'/' => self.name().map(Object::Name),
            b'(' => self.literal().map(Object::String),
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_white();
                    if self.peek()? == b']' {
                        self.pos += 1;
                        return Ok(Object::Array(items));
                    }
                    items.push(self.object_in(depth + 1)?);
                }
            }
            b'<' if self.buf.get(self.pos + 1) == Some(&b'<') => {
                self.pos += 2;
                self.dict(depth).map(Object::Dict)
            }
            b'<' if self.pos + 1 < self.buf.len() || self.complete => {
                self.hex_string().map(Object::String)
            }
            b'<' => Err(Stop::End),
            b if is_regular(b) => {
                let w = self.word()?;
                match w {
                    b"true" => Ok(Object::Bool(true)),
                    b"false" => Ok(Object::Bool(false)),
                    b"null" => Ok(Object::Null),
                    _ => match number(w) {
                        Some(Object::Int(n)) => self.maybe_ref(n),
                        Some(real) => Ok(real),
                        None => Err(Stop::Bad(at, "unexpected keyword")),
                    },
                }
            }
            _ => Err(Stop::Bad(at, "unexpected delimiter")),
        }
    }

    /// `n` alone, or the reference `n G R` it starts.
    fn maybe_ref(&mut self, n: i64) -> Lexed<Object> {
        let back = self.pos;
        let mut ahead = || -> Lexed<Option<Ref>> {
            let (Ok(num), Some(generation)) = (u32::try_from(n), number(self.word()?)) else {
                return Ok(None);
            };
            let generation = generation.as_int().and_then(|g| u16::try_from(g).ok());
            Ok(match (generation, self.word()?) {
                (Some(generation), b"R") => Some(Ref { num, generation }),
                _ => None,
            })
        };
        match ahead() {
            Ok(Some(r)) => Ok(Object::Ref(r)),
            Err(Stop::End) => Err(Stop::End),
            _ => {
                self.pos = back;
                Ok(Object::Int(n))
            }
        }
    }

    fn dict(&mut self, depth: usize) -> Lexed<Dict> {
        let mut pairs = Vec::new();
        loop {
            self.skip_white();
            match self.peek()? {
                b'>' => {
                    if self.buf.get(self.pos + 1).is_none() && !self.complete {
                        return Err(Stop::End);
                    }
                    if self.buf.get(self.pos + 1) != Some(&b'>') {
                        return Err(Stop::Bad(self.pos, "expected >>"));
                    }
                    self.pos += 2;
                    return Ok(Dict(pairs));
                }
                b'/' => {
                    let key = self.name()?;
                    let value = self.object_in(depth + 1)?;
                    pairs.push((key, value));
                }
                _ => return Err(Stop::Bad(self.pos, "expected a name")),
            }
        }
    }

    /// A `/Name`, its `#xx` escapes read.
    fn name(&mut self) -> Lexed<Vec<u8>> {
        self.pos += 1;
        let raw = self.run()?;
        let mut out = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            match (
                raw[i],
                raw.get(i + 1).and_then(|&b| hex(b)),
                raw.get(i + 2).and_then(|&b| hex(b)),
            ) {
                (b'#', Some(h), Some(l)) => {
                    out.push(h << 4 | l);
                    i += 3;
                }
                (b, _, _) => {
                    out.push(b);
                    i += 1;
                }
            }
        }
        Ok(out)
    }

    /// A `(…)` string: balanced parentheses, escapes, and any line end read as one `\n`.
    fn literal(&mut self) -> Lexed<Vec<u8>> {
        self.pos += 1;
        let (mut out, mut open) = (Vec::new(), 1);
        loop {
            let b = self.peek()?;
            self.pos += 1;
            match b {
                b'(' => {
                    open += 1;
                    out.push(b);
                }
                b')' => {
                    open -= 1;
                    if open == 0 {
                        return Ok(out);
                    }
                    out.push(b);
                }
                b'\r' => {
                    if self.peek()? == b'\n' {
                        self.pos += 1;
                    }
                    out.push(b'\n');
                }
                b'\\' => {
                    let e = self.peek()?;
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'0'..=b'7' => {
                            let mut v = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.peek()? {
                                    d @ b'0'..=b'7' => {
                                        v = v * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(v as u8);
                        }
                        // A backslash at a line's end joins the lines.
                        b'\r' => {
                            if self.peek()? == b'\n' {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        other => out.push(other),
                    }
                }
                _ => out.push(b),
            }
        }
    }

    /// A `<…>` string; an odd last digit is followed by 0.
    fn hex_string(&mut self) -> Lexed<Vec<u8>> {
        self.pos += 1;
        let (mut out, mut high) = (Vec::new(), None);
        loop {
            let b = self.peek()?;
            self.pos += 1;
            if b == b'>' {
                if let Some(h) = high {
                    out.push(h << 4);
                }
                return Ok(out);
            }
            if is_white(b) {
                continue;
            }
            let d = hex(b).ok_or(Stop::Bad(self.pos - 1, "not a hex digit"))?;
            match high.take() {
                Some(h) => out.push(h << 4 | d),
                None => high = Some(d),
            }
        }
    }

    /// `N G obj` and the object after it, and, for a stream, where its data starts: the caller
    /// reads that by its `/Length`. Whatever follows (`endobj`) is not asked for, as no reader
    /// asks for it.
    pub fn indirect(&mut self) -> Lexed<(Ref, Object, Option<usize>)> {
        let at = self.pos;
        let num = u32::try_from(self.unsigned()?).map_err(|_| Stop::Bad(at, "object number"))?;
        let generation =
            u16::try_from(self.unsigned()?).map_err(|_| Stop::Bad(at, "generation"))?;
        self.keyword(b"obj")?;
        let object = self.object()?;
        let back = self.pos;
        let data = match (&object, self.word()) {
            (Object::Dict(_), Ok(b"stream")) => {
                // The keyword's line ends in CRLF or LF; a lone CR is taken too.
                match (self.buf.get(self.pos), self.buf.get(self.pos + 1)) {
                    (Some(b'\r'), Some(b'\n')) => self.pos += 2,
                    (None, _) | (Some(b'\r'), None) if !self.complete => return Err(Stop::End),
                    (Some(b'\r' | b'\n'), _) => self.pos += 1,
                    _ => {}
                }
                Some(self.pos)
            }
            (_, Err(Stop::End)) => return Err(Stop::End),
            _ => {
                self.pos = back;
                None
            }
        };
        let object = match (object, data) {
            (Object::Dict(dict), Some(_)) => Object::Stream(crate::Stream {
                dict,
                raw: Vec::new(),
            }),
            (object, _) => object,
        };
        Ok((Ref { num, generation }, object, data))
    }
}

/// A number word: an integer, or a real (no exponent in PDF). An integer past `i64` is a real.
fn number(w: &[u8]) -> Option<Object> {
    let s = std::str::from_utf8(w).ok()?;
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    if !digits.contains('.') {
        return Some(match s.parse::<i64>() {
            Ok(i) => Object::Int(i),
            Err(_) => Object::Real(s.parse().ok()?),
        });
    }
    if digits.matches('.').count() > 1 || digits == "." {
        return None;
    }
    // `f64` takes neither `-.5` nor `5.` spelled with a sign before the point alone; PDF does.
    let sign = if s.starts_with('-') { -1.0 } else { 1.0 };
    let digits = format!("0{digits}0");
    Some(Object::Real(sign * digits.parse::<f64>().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &[u8]) -> Object {
        Lexer::new(s, 0, true).object().unwrap()
    }

    #[test]
    fn objects_read_as_the_spec_spells_them() {
        assert_eq!(parse(b"  -12 "), Object::Int(-12));
        assert_eq!(parse(b"-.5"), Object::Real(-0.5));
        assert_eq!(parse(b"4."), Object::Real(4.0));
        assert_eq!(parse(b"/A#20B"), Object::Name(b"A B".to_vec()));
        assert_eq!(
            parse(b"(a(b)\\)\\101\\\r\nc\r\nd)"),
            Object::String(b"a(b))Ac\nd".to_vec())
        );
        assert_eq!(parse(b"<48 6>"), Object::String(b"H`".to_vec()));
        assert_eq!(parse(b"[1 0 R 2 true]"), {
            let r = Object::Ref(Ref {
                num: 1,
                generation: 0,
            });
            Object::Array(vec![r, Object::Int(2), Object::Bool(true)])
        });
        let dict = parse(b"<</A 1 %comment\n/B<</C null>>>>");
        assert_eq!(dict.as_dict().unwrap().get(b"A"), Some(&Object::Int(1)));
        // A number last in a buffer that is all there is, as in an object stream, is no reference.
        assert_eq!(parse(b"7 0"), Object::Int(7));
    }

    #[test]
    fn a_window_that_ends_mid_object_asks_for_more() {
        for cut in [b"<</A 1".as_slice(), b"12", b"5 0", b"(abc", b"[1 2"] {
            assert_eq!(
                Lexer::new(cut, 0, false).object(),
                Err(Stop::End),
                "{cut:?}"
            );
        }
    }

    #[test]
    fn nesting_past_the_cap_is_refused() {
        let deep = [b"[".repeat(100), b"]".repeat(100)].concat();
        assert!(matches!(
            Lexer::new(&deep, 0, true).object(),
            Err(Stop::Bad(_, "nested too deep"))
        ));
    }
}
