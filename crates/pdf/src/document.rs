//! What a document says of itself and where it leads: its outline (ISO 32000-1 §12.3.3), its
//! information dictionary (§14.3.3) and its pages' links (§12.5.6.5), read from its objects, a
//! session's update over them included. Where a file is ambiguous these read it as pdfium does,
//! which the accent-core corpus test holds them to.

use std::collections::{HashMap, HashSet};

use crate::{Dict, Object, Pdf, Ref, Result, Update, text_string};

/// How deep the outline is read, and how many of its entries at most: no real document comes
/// near either.
const OUTLINE_DEPTH: usize = 32;
const OUTLINE_ENTRIES: usize = 10_000;
/// How deep a name tree is followed, as pdfium follows one.
const TREE_DEPTH: usize = 32;

/// A page of this document a bookmark or a link leads to, and the y its view wants at the top of
/// the window, in the page's own space (y up), for a view that names one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dest {
    pub page: usize,
    pub top: Option<f32>,
}

/// One entry of the outline, depth first: its title, blanks for control characters, and the
/// page it leads to.
#[derive(Debug, Clone, PartialEq)]
pub struct Bookmark {
    pub depth: usize,
    pub title: String,
    pub page: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Page(Dest),
    Uri(String),
}

/// A `/Link` annotation: its `/Rect`, `[left, bottom, right, top]` in the page's space, and where
/// it leads.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub rect: [f32; 4],
    pub target: Target,
}

/// What the information dictionary says, each entry trimmed and `None` where blank; the dates as
/// written (`D:YYYYMMDDHHmmSSOHH'mm`); and the version the header declares.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Info {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub created: Option<String>,
    pub modified: Option<String>,
    pub version: Option<String>,
}

/// A document as it now reads: a file's objects, and a session's update over them where there
/// is one.
pub struct Document<'a> {
    pdf: &'a Pdf,
    update: Option<&'a Update>,
    /// Each page's place by its object number, as pdfium finds a page.
    pages: HashMap<u32, usize>,
}

impl<'a> Document<'a> {
    /// `pdf`, `update` over it, its pages in the order `pages` lists them.
    pub fn new(pdf: &'a Pdf, update: Option<&'a Update>, pages: &[Ref]) -> Document<'a> {
        let pages = (pages.iter().enumerate())
            .map(|(i, r)| (r.num, i))
            .collect();
        Document { pdf, update, pages }
    }

    fn get(&self, r: Ref) -> Result<Object> {
        match self.update {
            Some(update) => update.get(self.pdf, r),
            None => self.pdf.get(r),
        }
    }

    /// `object`, or what it refers to; null for none.
    fn resolve(&self, object: Option<&Object>) -> Result<Object> {
        match object {
            Some(&Object::Ref(r)) => self.get(r),
            Some(o) => Ok(o.clone()),
            None => Ok(Object::Null),
        }
    }

    /// `key` of `dict` as a dictionary, a reference followed; empty for anything else.
    fn dict(&self, dict: &Dict, key: &[u8]) -> Result<Dict> {
        Ok(self
            .resolve(dict.get(key))?
            .as_dict()
            .cloned()
            .unwrap_or_default())
    }

    fn root(&self) -> Result<Dict> {
        self.dict(self.pdf.trailer(), b"Root")
    }

    /// The outline, flattened depth first; a node met again, as a loop meets it, is left out.
    pub fn outline(&self) -> Result<Vec<Bookmark>> {
        let outlines = self.dict(&self.root()?, b"Outlines")?;
        let (mut out, mut seen) = (Vec::new(), HashSet::new());
        let mut stack: Vec<(Object, usize)> = (outlines.get(b"First").cloned())
            .map(|first| (first, 0))
            .into_iter()
            .collect();
        // Pre-order: a node's sibling goes on before its child, so the child comes off first.
        while let Some((node, depth)) = stack.pop() {
            if out.len() >= OUTLINE_ENTRIES {
                break;
            }
            if node.reference().is_some_and(|r| !seen.insert(r)) {
                continue;
            }
            let Object::Dict(item) = self.resolve(Some(&node))? else {
                continue;
            };
            let title = match self.resolve(item.get(b"Title"))? {
                Object::String(s) => text_string(&s),
                _ => String::new(),
            };
            out.push(Bookmark {
                depth,
                // pdfium blanks control characters, a line break in a title among them.
                title: title
                    .chars()
                    .map(|c| if c > ' ' { c } else { ' ' })
                    .collect(),
                page: self.goes(&item)?.map(|dest| dest.page),
            });
            if let Some(next) = item.get(b"Next") {
                stack.push((next.clone(), depth));
            }
            if let Some(child) = item.get(b"First").filter(|_| depth + 1 < OUTLINE_DEPTH) {
                stack.push((child.clone(), depth + 1));
            }
        }
        Ok(out)
    }

    /// The links of page `page`, in its `/Annots` order: those leading to a page of this
    /// document or to a URI.
    pub fn links(&self, page: Ref) -> Result<Vec<Link>> {
        let page = self.get(page)?;
        let annots = self.resolve(page.as_dict().and_then(|p| p.get(b"Annots")))?;
        let mut out = Vec::new();
        for entry in annots.as_array().unwrap_or_default() {
            let Object::Dict(annot) = self.resolve(Some(entry))? else {
                continue;
            };
            if annot.get(b"Subtype").and_then(Object::as_name) != Some(b"Link") {
                continue;
            }
            let Some(rect) = self.rect(annot.get(b"Rect"))? else {
                continue;
            };
            let target = match self.dest_array(&annot)? {
                // A destination with no page here is no link, a URI action beside it or not.
                Some(dest) => match self.dest(&dest)? {
                    Some(dest) => Target::Page(dest),
                    None => continue,
                },
                None => match self.uri(&annot)? {
                    Some(uri) => Target::Uri(uri),
                    None => continue,
                },
            };
            out.push(Link { rect, target });
        }
        Ok(out)
    }

    /// The information dictionary.
    pub fn info(&self) -> Result<Info> {
        let info = self.dict(self.pdf.trailer(), b"Info")?;
        let text = |key: &[u8]| -> Result<Option<String>> {
            let text = match self.resolve(info.get(key))? {
                Object::String(s) | Object::Name(s) => text_string(&s),
                _ => return Ok(None),
            };
            let text = text.trim();
            Ok((!text.is_empty()).then(|| text.to_string()))
        };
        Ok(Info {
            title: text(b"Title")?,
            author: text(b"Author")?,
            subject: text(b"Subject")?,
            keywords: text(b"Keywords")?,
            creator: text(b"Creator")?,
            producer: text(b"Producer")?,
            created: text(b"CreationDate")?,
            modified: text(b"ModDate")?,
            version: self.version()?,
        })
    }

    /// The version the document keeps to: its header's, or the catalog's `/Version` where that
    /// is later (ISO 32000-1 §7.7.2).
    fn version(&self) -> Result<Option<String>> {
        let header = self.pdf.version();
        let catalog = match self.root()?.get(b"Version") {
            Some(Object::Name(v)) => crate::pdf::known_version(v),
            _ => None,
        };
        Ok(header.max(catalog))
    }

    /// Where an outline item or a link leads, by its `/Dest` or its GoTo action.
    fn goes(&self, item: &Dict) -> Result<Option<Dest>> {
        match self.dest_array(item)? {
            Some(dest) => self.dest(&dest),
            None => Ok(None),
        }
    }

    /// The destination array of an outline item or a link: its `/Dest`, named or explicit, or
    /// its `/A` GoTo action's `/D`. Another file's (GoToR) leads nowhere here.
    fn dest_array(&self, item: &Dict) -> Result<Option<Vec<Object>>> {
        if let Some(dest) = self.explicit(item.get(b"Dest"))? {
            return Ok(Some(dest));
        }
        let action = self.dict(item, b"A")?;
        match action.get(b"S").and_then(Object::as_name) {
            Some(b"GoTo") => self.explicit(action.get(b"D")),
            _ => Ok(None),
        }
    }

    /// `dest` as an array: itself, or the one a name or a string names.
    fn explicit(&self, dest: Option<&Object>) -> Result<Option<Vec<Object>>> {
        let named = match self.resolve(dest)? {
            Object::Array(a) => return Ok(Some(a)),
            Object::Name(n) | Object::String(n) => self.named(&n)?,
            _ => return Ok(None),
        };
        // A named destination is an array, or a dictionary holding one in `/D`.
        Ok(match named {
            Object::Array(a) => Some(a),
            Object::Dict(d) => self
                .resolve(d.get(b"D"))?
                .as_array()
                .map(<[Object]>::to_vec),
            _ => None,
        })
    }

    /// The destination named `name`: in the `/Dests` name tree of the `/Names` dictionary, else
    /// in the catalog's `/Dests` dictionary (PDF 1.1).
    fn named(&self, name: &[u8]) -> Result<Object> {
        let root = self.root()?;
        let tree = self.dict(&self.dict(&root, b"Names")?, b"Dests")?;
        let found = self.find(&tree, name, 0, &mut HashSet::new())?;
        match found {
            Some(found) => Ok(found),
            None => self.resolve(self.dict(&root, b"Dests")?.get(name)),
        }
    }

    /// `key` in the name tree under `node`, its `/Limits` passing by kids that cannot hold it.
    fn find(
        &self,
        node: &Dict,
        key: &[u8],
        depth: usize,
        seen: &mut HashSet<Ref>,
    ) -> Result<Option<Object>> {
        if let Object::Array(names) = self.resolve(node.get(b"Names"))? {
            for [name, value] in names.as_chunks::<2>().0 {
                if matches!(self.resolve(Some(name))?, Object::String(k) if k == key) {
                    return Ok(Some(self.resolve(Some(value))?));
                }
            }
        }
        if depth >= TREE_DEPTH {
            return Ok(None);
        }
        let Object::Array(kids) = self.resolve(node.get(b"Kids"))? else {
            return Ok(None);
        };
        for kid in &kids {
            if kid.reference().is_some_and(|r| !seen.insert(r)) {
                continue;
            }
            let Object::Dict(kid) = self.resolve(Some(kid))? else {
                continue;
            };
            if let Object::Array(limits) = self.resolve(kid.get(b"Limits"))?
                && let [Object::String(low), Object::String(high)] = &limits[..]
                && (key < &low[..] || key > &high[..])
            {
                continue;
            }
            if let Some(found) = self.find(&kid, key, depth + 1, seen)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    /// The page `dest` names, by its object, or by its number as some writers put it, and the y
    /// its view wants at the top: an XYZ's top, a FitH's or a FitR's.
    fn dest(&self, dest: &[Object]) -> Result<Option<Dest>> {
        let page = match dest.first() {
            Some(&Object::Ref(r)) => self.pages.get(&r.num).copied(),
            Some(&Object::Int(n)) => usize::try_from(n).ok(),
            _ => None,
        };
        let Some(page) = page else {
            return Ok(None);
        };
        let at = match dest.get(1).and_then(Object::as_name) {
            Some(b"XYZ") => 3,
            Some(b"FitH") => 2,
            Some(b"FitR") => 5,
            _ => 0,
        };
        let top = match at {
            0 => None,
            at => number(&self.resolve(dest.get(at))?),
        };
        Ok(Some(Dest { page, top }))
    }

    /// A URI action's URI, a relative one after the catalog's `/URI /Base`.
    fn uri(&self, annot: &Dict) -> Result<Option<String>> {
        let action = self.dict(annot, b"A")?;
        if action.get(b"S").and_then(Object::as_name) != Some(b"URI") {
            return Ok(None);
        }
        let Object::String(mut uri) = self.resolve(action.get(b"URI"))? else {
            return Ok(None);
        };
        let scheme = uri.iter().position(|&b| b == b':');
        if scheme.is_none_or(|at| at == 0)
            && let Object::String(base) =
                self.resolve(self.dict(&self.root()?, b"URI")?.get(b"Base"))?
        {
            uri = [base, uri].concat();
        }
        // ASCII as the spec has it; one that is not even UTF-8 is no link, as pdfium-render
        // reads it.
        Ok(String::from_utf8(uri).ok())
    }

    /// A rectangle, normalised; `None` for anything but four numbers.
    fn rect(&self, rect: Option<&Object>) -> Result<Option<[f32; 4]>> {
        let Object::Array(values) = self.resolve(rect)? else {
            return Ok(None);
        };
        let mut n = Vec::with_capacity(4);
        for v in &values {
            n.extend(number(&self.resolve(Some(v))?));
        }
        let [x0, y0, x1, y1] = n[..] else {
            return Ok(None);
        };
        Ok(Some([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)]))
    }
}

fn number(o: &Object) -> Option<f32> {
    match *o {
        Object::Int(i) => Some(i as f32),
        Object::Real(r) => Some(r as f32),
        _ => None,
    }
}
