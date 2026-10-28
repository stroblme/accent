//! The objects an edit reads and writes: the file through `accent_pdf`'s reader, and the
//! session's update after it, which is what a save appends and pdfium is opened again over.

use std::collections::HashSet;
use std::sync::Arc;

use accent_pdf::{Dict, Object, Pdf, Ref, Update};

use crate::{Error, Result};

impl From<accent_pdf::Error> for Error {
    fn from(e: accent_pdf::Error) -> Error {
        Error::Pdf(e.to_string())
    }
}

/// The document as the session has it: the file's objects, the session's own over them, and the
/// pages in order. Cloned, it is a copy the document never sees (`copy_with_highlights`).
#[derive(Clone)]
pub(super) struct Objects {
    pub(super) pdf: Arc<Pdf>,
    pub(super) update: Update,
    /// Every page's object, in the order of the page tree.
    pub(super) pages: Vec<Ref>,
    /// Whether the page tree is one node, as the first page edit makes it (`Objects::flatten`).
    pub(super) flat: bool,
}

impl Objects {
    /// The session over a file just read, its pages listed.
    pub(super) fn new(pdf: Pdf, pages: Vec<Ref>) -> Objects {
        Objects {
            update: Update::new(&pdf),
            pdf: Arc::new(pdf),
            pages,
            flat: false,
        }
    }

    /// `r` as the session has it.
    pub(super) fn get(&self, r: Ref) -> Result<Object> {
        Ok(self.update.get(&self.pdf, r)?)
    }

    /// `object`, or what it refers to.
    pub(super) fn resolve(&self, object: &Object) -> Result<Object> {
        match object {
            Object::Ref(r) => self.get(*r),
            direct => Ok(direct.clone()),
        }
    }

    /// The dictionary `r` names (a stream's, for a stream).
    pub(super) fn dict(&self, r: Ref) -> Result<Dict> {
        (self.get(r)?.as_dict().cloned())
            .ok_or_else(|| Error::Pdf(format!("object {} is no dictionary", r.num)))
    }

    /// Page `page`'s object.
    pub(super) fn page(&self, page: usize) -> Result<Ref> {
        (self.pages.get(page).copied()).ok_or_else(|| Error::Invalid(format!("no page {page}")))
    }

    /// `key` of `page`, or of the nearest node above it that has it: what a page inherits.
    pub(super) fn inherited(&self, page: Ref, key: &[u8]) -> Result<Option<Object>> {
        let (mut node, mut seen) = (page, HashSet::new());
        while seen.insert(node) {
            let dict = self.dict(node)?;
            if let Some(value) = dict.get(key) {
                return Ok(Some(value.clone()));
            }
            match dict.get(b"Parent").and_then(Object::reference) {
                Some(parent) => node = parent,
                None => return Ok(None),
            }
        }
        Err(Error::Pdf("the page tree loops".to_string()))
    }

    /// Page `page`'s `/Annots` entries, as it lists them: pdfium's annotation indices are these.
    pub(super) fn annots(&self, page: usize) -> Result<Vec<Object>> {
        let dict = self.dict(self.page(page)?)?;
        Ok(match dict.get(b"Annots") {
            Some(list) => self.resolve(list)?.as_array().unwrap_or_default().to_vec(),
            None => Vec::new(),
        })
    }

    /// Page `page`'s `/Annots` made `entries`: the array rewritten where it is an object of its
    /// own, else made one, so the next edit writes the array and not the page.
    pub(super) fn set_annots(&mut self, page: usize, entries: Vec<Object>) -> Result<()> {
        let at = self.page(page)?;
        let mut dict = self.dict(at)?;
        match dict.get(b"Annots") {
            Some(&Object::Ref(list)) => self.update.set(list, Object::Array(entries)),
            _ => {
                let list = self.update.add(Object::Array(entries));
                dict.set(b"Annots", Object::Ref(list));
                self.update.set(at, Object::Dict(dict));
            }
        }
        Ok(())
    }

    /// `annot` added to the end of page `page`'s `/Annots`, with `/P` naming the page.
    pub(super) fn append_annot(&mut self, page: usize, mut annot: Dict) -> Result<Ref> {
        annot.set(b"P", Object::Ref(self.page(page)?));
        let at = self.update.add(Object::Dict(annot));
        let mut entries = self.annots(page)?;
        entries.push(Object::Ref(at));
        self.set_annots(page, entries)?;
        Ok(at)
    }
}

/// A number, whether the file wrote it as an integer or a real.
pub(super) fn number(o: &Object) -> Option<f32> {
    match o {
        Object::Int(i) => Some(*i as f32),
        Object::Real(r) => Some(*r as f32),
        _ => None,
    }
}

pub(super) fn real(v: f32) -> Object {
    Object::Real(f64::from(v))
}

pub(super) fn name(n: &[u8]) -> Object {
    Object::Name(n.to_vec())
}

/// Reals as an array, as a rectangle or a list of points is written.
pub(super) fn reals(values: impl IntoIterator<Item = f32>) -> Object {
    Object::Array(values.into_iter().map(real).collect())
}

pub(super) fn dict<const N: usize>(entries: [(&[u8], Object); N]) -> Dict {
    accent_pdf::Dict(entries.map(|(k, v)| (k.to_vec(), v)).into())
}

/// A text string as PDF reads one (ISO 32000-1 §7.9.2.2): plain bytes for ASCII, else UTF-16BE
/// after its byte order mark.
pub(super) fn text(s: &str) -> Object {
    match s.is_ascii() {
        true => Object::String(s.as_bytes().to_vec()),
        false => {
            let units = s.encode_utf16().flat_map(u16::to_be_bytes);
            Object::String([0xfe, 0xff].into_iter().chain(units).collect())
        }
    }
}
