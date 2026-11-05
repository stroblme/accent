//! Object streams (ISO 32000-1 §7.5.7): many objects compressed together, each found by the
//! number and offset pairs the stream starts with.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::lex::Lexer;
use crate::{Error, Object, Result};

/// What a document's decoded object streams may hold together: writers pack 100 to 255 small
/// objects into each, so 93 TeX distribution manuals came to 5.3 MiB at most (2026-11-05).
pub(crate) const BUDGET: usize = 8 << 20;

/// The object streams a document keeps decoded, within [`BUDGET`], the least recently read
/// dropped first.
#[derive(Default)]
pub(crate) struct Cache {
    /// Each stream by its number, with the tick it was last read at.
    streams: HashMap<u32, (Arc<ObjStm>, u64)>,
    /// The streams by the tick they were last read at, the least recent first.
    order: BTreeMap<u64, u32>,
    tick: u64,
    /// What the streams hold together ([`ObjStm::size`]).
    pub bytes: usize,
}

impl Cache {
    /// Stream `num`, now the most recently read.
    pub fn get(&mut self, num: u32) -> Option<Arc<ObjStm>> {
        let (stm, tick) = self.streams.get_mut(&num)?;
        self.order.remove(tick);
        self.tick += 1;
        *tick = self.tick;
        self.order.insert(self.tick, num);
        Some(stm.clone())
    }

    /// `stm` kept as stream `num`, the least recently read others dropped until the rest fit. A
    /// stream past the budget on its own stays alone, since decoding it again for each of its
    /// objects would cost more than keeping what its decoding allocated anyway.
    pub fn insert(&mut self, num: u32, stm: Arc<ObjStm>) {
        self.tick += 1;
        self.bytes += stm.size();
        if let Some((old, tick)) = self.streams.insert(num, (stm, self.tick)) {
            self.order.remove(&tick);
            self.bytes -= old.size();
        }
        self.order.insert(self.tick, num);
        while self.bytes > BUDGET
            && self.streams.len() > 1
            && let Some((_, oldest)) = self.order.pop_first()
            && let Some((old, _)) = self.streams.remove(&oldest)
        {
            self.bytes -= old.size();
        }
    }
}

/// A decoded object stream.
pub(crate) struct ObjStm {
    /// Each object's number and where it starts in `data`.
    index: Vec<(u32, u32)>,
    data: Vec<u8>,
}

impl ObjStm {
    /// The stream with dictionary `/N` and `/First`, already decoded.
    pub fn new(dict: &crate::Dict, mut data: Vec<u8>) -> Result<ObjStm> {
        let get = |key: &[u8]| {
            dict.get(key)
                .and_then(Object::as_int)
                .and_then(|v| usize::try_from(v).ok())
        };
        let (Some(n), Some(first)) = (get(b"N"), get(b"First")) else {
            return Err(Error::Broken("object stream without /N or /First".into()));
        };
        let mut lex = Lexer::new(&data, 0, true);
        // Each pair takes four bytes at least and eight in the index, which caps a lying `/N` at
        // an index twice the data.
        let count = n.min(data.len() / 4);
        let mut index = Vec::with_capacity(count);
        for _ in 0..count {
            let pair = lex.unsigned().and_then(|num| Ok((num, lex.unsigned()?)));
            let Ok((num, off)) = pair else {
                return Err(Error::Broken("object stream index".into()));
            };
            let at = first.checked_add(off as usize).map(u32::try_from);
            let (Ok(num), Some(Ok(at))) = (u32::try_from(num), at) else {
                return Err(Error::Broken("object stream index".into()));
            };
            index.push((num, at));
        }
        // It is cached: the spare capacity inflating left, up to as much again as the data, goes.
        data.shrink_to_fit();
        Ok(ObjStm { index, data })
    }

    /// The bytes it holds: its data and its index.
    pub fn size(&self) -> usize {
        self.data.capacity() + self.index.capacity() * size_of::<(u32, u32)>()
    }

    /// The numbers of the objects it holds, in order.
    pub fn numbers(&self) -> impl Iterator<Item = u32> {
        self.index.iter().map(|&(num, _)| num)
    }

    /// Object `num`, the `i`th of the stream.
    pub fn get(&self, i: usize, num: u32) -> Result<Object> {
        match self.index.get(i) {
            Some(&(n, at)) if n == num && at as usize <= self.data.len() => {
                Lexer::new(&self.data, at as usize, true)
                    .object()
                    .map_err(|_| Error::Broken(format!("object {num} in its object stream")))
            }
            _ => Err(Error::Broken(format!(
                "object {num} is not where the xref puts it"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Dict;

    #[test]
    fn a_lying_count_indexes_at_most_twice_the_data() {
        let data = b"0 0 ".repeat(1 << 18);
        let len = data.len();
        let dict = Dict(vec![
            (b"N".to_vec(), Object::Int(1 << 40)),
            (b"First".to_vec(), Object::Int(len as i64)),
        ]);
        let stm = ObjStm::new(&dict, data).unwrap();
        assert_eq!(stm.numbers().count(), len / 4);
        // The data, and an index of at most twice its size.
        assert!(stm.size() <= 3 * len);
    }
}
