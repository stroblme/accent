//! Page insertion, deletion, movement and import, written into the page tree of the session.

use std::collections::HashMap;

use accent_pdf::{Dict, Object, Ref, Stream, Update};

use crate::{Error, Result};

use super::PdfDoc;
use super::cos::{Objects, dict, name, reals};

impl PdfDoc {
    /// A blank page at `at`, the size of the page before it (the first page's, at the front):
    /// what a paper notebook does when the page runs out, and what keeps a drawing readable in any
    /// viewer — one MediaBox per page, none of them growing. `at` may be the page count, which
    /// appends.
    pub fn insert_page(&mut self, at: usize) -> Result<()> {
        let count = self.cos.pages.len();
        if count == 0 || at > count {
            return Err(Error::Invalid(format!(
                "cannot insert page {at} into {count}"
            )));
        }
        let size = self.page_size(at.saturating_sub(1))?;
        self.cos.insert_blank(at, size)?;
        self.reopen()
    }

    /// Take a page out. Refused for the last one: a PDF without pages is not one a reader opens.
    pub fn delete_page(&mut self, page: usize) -> Result<()> {
        self.take_pages(page, 1).map(drop)
    }

    /// Take `count` pages out from `at`, and the pages taken, which [`PdfDoc::put_pages`] puts
    /// back as themselves: the page objects stay in the file, out of the page tree, so whatever
    /// names them — a link, a bookmark — names them again once they are back.
    pub(super) fn take_pages(&mut self, at: usize, count: usize) -> Result<Vec<Ref>> {
        let pages = self.cos.pages.len();
        if count >= pages {
            return Err(Error::Invalid("a PDF keeps at least one page".to_string()));
        }
        if at + count > pages {
            return Err(Error::Invalid(format!(
                "cannot take pages {at}..{} of {pages}",
                at + count
            )));
        }
        let taken = self
            .cos
            .repage(|pages| pages.drain(at..at + count).collect())?;
        self.reopen()?;
        Ok(taken)
    }

    /// Put pages [`PdfDoc::take_pages`] took, or an import made, back in at `at`.
    pub(super) fn put_pages(&mut self, at: usize, pages: &[Ref]) -> Result<()> {
        if at > self.cos.pages.len() {
            return Err(Error::Invalid(format!("cannot put pages back at {at}")));
        }
        self.cos.repage(|all| {
            all.splice(at..at, pages.iter().copied());
        })?;
        self.reopen()
    }

    /// Insert all pages of `source` at `at` in order, returning their count. `at` may equal the
    /// page count to append. Each page comes with everything it uses — text, annotations, ink —
    /// and links between the pages that came lead to the copies; a link to anything else and the
    /// bookmarks of `source` do not come. A form widget is cut loose from its field.
    pub fn import_pages(&mut self, source: &PdfDoc, at: usize) -> Result<usize> {
        let count = self.cos.pages.len();
        if at > count {
            return Err(Error::Invalid(format!(
                "cannot import pages at {at} of {count}"
            )));
        }
        let added = self.cos.import(&source.cos, at)?;
        self.reopen()?;
        Ok(added)
    }

    /// Move the page itself from `from` to `to`, so the links and bookmarks into it follow it.
    pub fn move_page(&mut self, from: usize, to: usize) -> Result<()> {
        let count = self.cos.pages.len();
        if from >= count || to >= count {
            return Err(Error::Invalid(format!(
                "cannot move page {from} to {to} of {count}"
            )));
        }
        self.cos.repage(|pages| {
            let page = pages.remove(from);
            pages.insert(to, page);
        })?;
        self.reopen()
    }
}

/// What a page inherits from the nodes above it in the page tree (ISO 32000-1 §7.7.3.4).
const INHERITED: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

impl Objects {
    /// The pages changed by `edit`, and written into the page tree.
    fn repage<T>(&mut self, edit: impl FnOnce(&mut Vec<Ref>) -> T) -> Result<T> {
        let root = self.flatten()?;
        let made = edit(&mut self.pages);
        let mut node = self.dict(root)?;
        let kids = self.pages.iter().map(|&page| Object::Ref(page)).collect();
        node.set(b"Kids", Object::Array(kids));
        node.set(b"Count", Object::Int(self.pages.len() as i64));
        self.update.set(root, Object::Dict(node));
        Ok(made)
    }

    /// The page tree's root, made, once, the one node every page names as its parent, a page
    /// that was under another node given what it inherited there: a page edit rewrites it as one
    /// list of pages.
    fn flatten(&mut self) -> Result<Ref> {
        let root = (self.pdf.root()?.get(b"Pages").and_then(Object::reference))
            .ok_or_else(|| Error::Pdf("no page tree".to_string()))?;
        if self.flat {
            return Ok(root);
        }
        for page in self.pages.clone() {
            let mut node = self.dict(page)?;
            if node.get(b"Parent").and_then(Object::reference) == Some(root) {
                continue;
            }
            for key in INHERITED {
                if node.get(key).is_none()
                    && let Some(value) = self.inherited(page, key)?
                {
                    node.set(key, value);
                }
            }
            node.set(b"Parent", Object::Ref(root));
            self.update.set(page, Object::Dict(node));
        }
        self.flat = true;
        Ok(root)
    }

    /// A blank page `size` points across at `at`.
    fn insert_blank(&mut self, at: usize, (width, height): (f32, f32)) -> Result<()> {
        let root = self.flatten()?;
        let page = self.update.add(Object::Dict(dict([
            (b"Type", name(b"Page")),
            (b"Parent", Object::Ref(root)),
            (b"MediaBox", reals([0.0, 0.0, width, height])),
            (b"Resources", Object::Dict(Dict::default())),
        ])));
        self.repage(|pages| pages.insert(at, page))
    }

    /// Every page of `source` copied in at `at`, and how many.
    fn import(&mut self, source: &Objects, at: usize) -> Result<usize> {
        let root = self.flatten()?;
        let mut copy = Copier {
            from: source,
            into: &mut self.update,
            made: HashMap::new(),
            todo: Vec::new(),
        };
        // Numbered first, so a link from one page to another that comes too finds its copy.
        let pages: Vec<Ref> = (source.pages.iter())
            .map(|&page| {
                let made = copy.into.add(Object::Null);
                copy.made.insert(page, made);
                made
            })
            .collect();
        for (&from, &to) in source.pages.iter().zip(&pages) {
            let mut page = source.dict(from)?;
            for key in INHERITED {
                if page.get(key).is_none()
                    && let Some(value) = source.inherited(from, key)?
                {
                    page.set(key, value);
                }
            }
            page.remove(b"Parent");
            let mut page = copy.dict(&page, false)?;
            page.set(b"Parent", Object::Ref(root));
            copy.into.set(to, Object::Dict(page));
        }
        while let Some((to, object)) = copy.todo.pop() {
            let copied = copy.object(&object)?;
            copy.into.set(to, copied);
        }
        let added = pages.len();
        self.repage(|all| {
            all.splice(at..at, pages);
        })?;
        Ok(added)
    }
}

/// A deep copy of objects from one document into another's update, each copied once.
struct Copier<'a> {
    from: &'a Objects,
    into: &'a mut Update,
    /// Each object of `from` reached so far, and its number here.
    made: HashMap<Ref, Ref>,
    /// Objects numbered here and still to be copied, as `from` has them.
    todo: Vec<(Ref, Object)>,
}

impl Copier<'_> {
    /// `object` with every reference in it leading to a copy, as [`Copier::dict`] says.
    fn object(&mut self, object: &Object) -> Result<Object> {
        Ok(match object {
            Object::Ref(r) => self.reference(*r)?,
            Object::Array(items) => Object::Array(
                (items.iter())
                    .map(|item| self.object(item))
                    .collect::<Result<_>>()?,
            ),
            Object::Dict(d) => Object::Dict(self.dict(d, false)?),
            Object::Stream(s) => Object::Stream(Stream {
                dict: self.dict(&s.dict, true)?,
                raw: s.raw.clone(),
            }),
            plain => plain.clone(),
        })
    }

    /// The copy of the object `r` names, numbered here at once and copied later; nothing for a
    /// page that is not among those copied, which would bring its document's page tree along.
    fn reference(&mut self, r: Ref) -> Result<Object> {
        if let Some(&made) = self.made.get(&r) {
            return Ok(Object::Ref(made));
        }
        let object = self.from.get(r)?;
        let kind = object.as_dict().and_then(|d| d.get(b"Type")?.as_name());
        if matches!(kind, Some(b"Page" | b"Pages")) {
            return Ok(Object::Null);
        }
        let made = self.into.add(Object::Null);
        self.made.insert(r, made);
        self.todo.push((made, object));
        Ok(Object::Ref(made))
    }

    /// `d` copied, less what names the other document's structure: its structure tree's keys,
    /// a form widget's field, a stream's `/Length` (the writer gives each its own), and a link
    /// or a go-to action into a page that did not come.
    fn dict(&mut self, d: &Dict, stream: bool) -> Result<Dict> {
        let widget = d.get(b"Subtype").and_then(Object::as_name) == Some(b"Widget");
        let mut out = Dict::default();
        for (key, value) in &d.0 {
            let skip = match key.as_slice() {
                b"StructParent" | b"StructParents" => true,
                b"Parent" => widget,
                b"Length" => stream,
                _ => false,
            };
            if skip {
                continue;
            }
            let value = self.object(value)?;
            if matches!(key.as_slice(), b"Dest" | b"A") && leads_nowhere(&value) {
                continue;
            }
            out.0.push((key.clone(), value));
        }
        Ok(out)
    }
}

/// Whether a copied destination, or a go-to action holding one, names a page that did not come.
fn leads_nowhere(value: &Object) -> bool {
    let dest = match value.as_dict() {
        Some(action) if action.get(b"S").and_then(Object::as_name) == Some(b"GoTo") => {
            action.get(b"D")
        }
        Some(_) => None,
        None => Some(value),
    };
    matches!(dest.and_then(Object::as_array), Some([Object::Null, ..]))
}
