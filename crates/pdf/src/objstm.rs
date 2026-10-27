//! Object streams (ISO 32000-1 §7.5.7): many objects compressed together, each found by the
//! number and offset pairs the stream starts with.

use crate::lex::Lexer;
use crate::{Error, Object, Result};

/// A decoded object stream.
pub(crate) struct ObjStm {
    /// Each object's number and where it starts in `data`.
    index: Vec<(u32, usize)>,
    data: Vec<u8>,
}

impl ObjStm {
    /// The stream with dictionary `/N` and `/First`, already decoded.
    pub fn new(dict: &crate::Dict, data: Vec<u8>) -> Result<ObjStm> {
        let get = |key: &[u8]| {
            dict.get(key)
                .and_then(Object::as_int)
                .and_then(|v| usize::try_from(v).ok())
        };
        let (Some(n), Some(first)) = (get(b"N"), get(b"First")) else {
            return Err(Error::Broken("object stream without /N or /First".into()));
        };
        let mut lex = Lexer::new(&data, 0, true);
        let mut index = Vec::new();
        // Each pair takes four bytes at least, which caps a lying `/N`.
        for _ in 0..n.min(data.len() / 4) {
            let pair = lex.unsigned().and_then(|num| Ok((num, lex.unsigned()?)));
            let Ok((num, off)) = pair else {
                return Err(Error::Broken("object stream index".into()));
            };
            let (Ok(num), Some(at)) = (u32::try_from(num), first.checked_add(off as usize)) else {
                return Err(Error::Broken("object stream index".into()));
            };
            index.push((num, at));
        }
        Ok(ObjStm { index, data })
    }

    /// Object `num`, the `i`th of the stream.
    pub fn get(&self, i: usize, num: u32) -> Result<Object> {
        match self.index.get(i) {
            Some(&(n, at)) if n == num && at <= self.data.len() => Lexer::new(&self.data, at, true)
                .object()
                .map_err(|_| Error::Broken(format!("object {num} in its object stream"))),
            _ => Err(Error::Broken(format!(
                "object {num} is not where the xref puts it"
            ))),
        }
    }
}
