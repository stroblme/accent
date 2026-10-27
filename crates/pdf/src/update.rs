//! One editing session's incremental update (ISO 32000-1 §7.5.6): the objects it adds and
//! changes, written after the file unchanged as one section, in the file's own xref style.

use std::collections::BTreeMap;

use crate::write;
use crate::{Dict, Object, Pdf, Ref, Result};

/// The trailer keys a section writes itself, or that belong to an xref stream's dictionary.
const OWN: &[&[u8]] = &[
    b"Prev",
    b"XRefStm",
    b"Size",
    b"Type",
    b"W",
    b"Index",
    b"Filter",
    b"DecodeParms",
    b"Length",
    b"DL",
];

pub struct Update {
    /// The file before this session: its size, and whether a line end closes it.
    base_len: u64,
    base_ends_line: bool,
    /// The base's newest xref, which the section names as its `/Prev`, and its style.
    prev: u64,
    stream: bool,
    /// The base's newest trailer less the keys of [`OWN`].
    trailer: Dict,
    /// The number the next new object gets.
    next: u32,
    /// The session's objects, each serialised once, when added or set.
    objects: BTreeMap<u32, (Ref, Object, Vec<u8>)>,
}

impl Update {
    /// An empty session over `base`.
    pub fn new(base: &Pdf) -> Update {
        let base_len = base.src.size();
        let last = base
            .src
            .read_vec(base_len.saturating_sub(1), 1)
            .unwrap_or_default();
        let mut trailer = base.trailer().clone();
        for key in OWN {
            trailer.remove(key);
        }
        // `/Size` from the trailer, unless an entry says the file has more.
        let size = base
            .trailer()
            .get(b"Size")
            .and_then(Object::as_int)
            .unwrap_or(0);
        let highest = base.xref.entries.keys().max().map_or(0, |n| n + 1);
        Update {
            base_len,
            base_ends_line: matches!(last.as_slice(), [b'\n' | b'\r']),
            prev: base.startxref(),
            stream: base.xref_is_stream(),
            trailer,
            next: u32::try_from(size).unwrap_or(0).max(highest).max(1),
            objects: BTreeMap::new(),
        }
    }

    /// `object` as a new object of the session.
    pub fn add(&mut self, object: Object) -> Ref {
        let r = Ref {
            num: self.next,
            generation: 0,
        };
        self.next += 1;
        self.set(r, object);
        r
    }

    /// `r` replaced by `object` for the rest of the session.
    pub fn set(&mut self, r: Ref, object: Object) {
        let mut bytes = Vec::new();
        write::indirect(&mut bytes, r, &object);
        self.objects.insert(r.num, (r, object, bytes));
    }

    /// `r` as the session has it: its own version, else the file's.
    pub fn get(&self, base: &Pdf, r: Ref) -> Result<Object> {
        match self.objects.get(&r.num) {
            Some((own, object, _)) if *own == r => Ok(object.clone()),
            _ => base.get(r),
        }
    }

    /// The section that, written after the base, makes it the session's document: the objects in
    /// number order, an xref of them, the trailer, `startxref`.
    pub fn section(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.base_ends_line {
            out.push(b'\n');
        }
        let mut rows = Vec::new();
        for (r, _, bytes) in self.objects.values() {
            rows.push((*r, self.base_len + out.len() as u64));
            out.extend(bytes);
        }
        let xref = self.base_len + out.len() as u64;
        let mut trailer = self.trailer.clone();
        trailer.set(b"Prev", Object::Int(self.prev as i64));
        match self.stream {
            false => {
                trailer.set(b"Size", Object::Int(i64::from(self.next)));
                write::table(&mut out, &rows, false);
                out.extend(b"trailer\n");
                write::object(&mut out, &Object::Dict(trailer));
                out.push(b'\n');
            }
            true => {
                // The xref stream is an object of its own, the next number, listed in itself.
                let own = Ref {
                    num: self.next,
                    generation: 0,
                };
                rows.push((own, xref));
                trailer.set(b"Size", Object::Int(i64::from(self.next) + 1));
                write::indirect(&mut out, own, &Object::Stream(xref_stream(&rows, trailer)));
            }
        }
        out.extend(format!("startxref\n{xref}\n%%EOF\n").bytes());
        out
    }
}

/// An xref stream of `rows` with `trailer`'s keys: type 1 entries, the offset field as wide as
/// the largest offset needs, unfiltered.
fn xref_stream(rows: &[(Ref, u64)], mut trailer: Dict) -> crate::Stream {
    let largest = rows.iter().map(|(_, offset)| *offset).max().unwrap_or(0);
    let width = (8 - largest.leading_zeros() as usize / 8).max(1);
    let (mut index, mut data) = (Vec::new(), Vec::new());
    for run in rows.chunk_by(|a, b| b.0.num == a.0.num + 1) {
        index.extend([
            Object::Int(i64::from(run[0].0.num)),
            Object::Int(run.len() as i64),
        ]);
        for (r, offset) in run {
            data.push(1);
            data.extend(&offset.to_be_bytes()[8 - width..]);
            data.extend(r.generation.to_be_bytes());
        }
    }
    let int = |n: usize| Object::Int(n as i64);
    trailer.set(b"Type", Object::Name(b"XRef".to_vec()));
    trailer.set(b"W", Object::Array(vec![int(1), int(width), int(2)]));
    trailer.set(b"Index", Object::Array(index));
    crate::Stream {
        dict: trailer,
        raw: data,
    }
}
