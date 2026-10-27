//! accent's own PDF object layer: a lazy reader over a file's revisions, the base of an
//! incremental writer that is to take every PDF write from pdfium, which then only renders and
//! reads text (DESIGN.md, Architecture, Crates).
//!
//! Pure Rust and free of `unsafe`, so it is tested on its own and builds for the musl host.

#![forbid(unsafe_code)]

mod filter;
mod lex;
mod objstm;
mod pdf;
mod source;
mod xref;

pub use pdf::Pdf;
pub use source::Source;

/// An indirect object's name: its number and generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ref {
    pub num: u32,
    pub generation: u16,
}

/// A PDF object as the file spells it. A stream keeps its data encoded ([`Pdf::decode`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Name(Vec<u8>),
    String(Vec<u8>),
    Array(Vec<Object>),
    Dict(Dict),
    Stream(Stream),
    Ref(Ref),
}

impl Object {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Object::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_name(&self) -> Option<&[u8]> {
        match self {
            Object::Name(n) => Some(n),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Object]> {
        match self {
            Object::Array(a) => Some(a),
            _ => None,
        }
    }

    /// A dictionary, or a stream's.
    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            Object::Dict(d) => Some(d),
            Object::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }

    pub fn reference(&self) -> Option<Ref> {
        match self {
            Object::Ref(r) => Some(*r),
            _ => None,
        }
    }
}

/// A dictionary in the file's key order, so one rewritten keeps it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Dict(pub Vec<(Vec<u8>, Object)>);

impl Dict {
    /// The value of `key`; the first, should a damaged file repeat it.
    pub fn get(&self, key: &[u8]) -> Option<&Object> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

/// A stream: its dictionary and its data as the file holds it, filters and all.
#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub dict: Dict,
    pub raw: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Not PDF syntax at byte `at` of the file.
    #[error("PDF syntax at byte {at}: {what}")]
    Syntax { at: u64, what: String },
    /// Readable, but not what its own structure says: an xref entry naming the wrong object, a
    /// loop. A writer must not append to such a file.
    #[error("damaged PDF: {0}")]
    Broken(String),
    #[error("unsupported PDF: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests;
