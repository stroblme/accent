//! A PDF read lazily: the xref at open, each object when it is asked for.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::crypt::Crypt;
use crate::objstm::{Cache, ObjStm};
use crate::xref::{self, Entry, Xref};
use crate::{Dict, Error, Object, Ref, Result, Source, Stream};

/// How deep one lookup may lead into others: a `/Length` held in an object stream, whose own
/// `/Length` is another object. Past it a file is taken to loop.
const DEPTH: usize = 16;

pub struct Pdf {
    pub(crate) src: Arc<dyn Source>,
    pub(crate) xref: Xref,
    pub(crate) objstms: Mutex<Cache>,
    /// The security handler of an encrypted file, and the `/Encrypt` dictionary's own number,
    /// which it leaves alone.
    pub(crate) crypt: Option<Crypt>,
    encryption: Option<Ref>,
}

impl Pdf {
    /// Read the xref chain of `src`. Shared, because the document a session edits is this base
    /// with the session's update after it, which pdfium reads as one (`accent_core::pdf`).
    /// An encrypted file opens where its user password is empty, as pdfium opens it.
    pub fn open(src: Arc<dyn Source>) -> Result<Pdf> {
        let xref = xref::read(&*src)?;
        Pdf::over(src, xref)
    }

    /// `src` read through an xref made again from its objects ([`xref::scan`]), as pdfium and
    /// other readers repair a file whose own xref does not read: to read what a damaged file says.
    /// Nothing may be appended to it, since an update names the file's own xref, which this is
    /// not.
    ///
    // ponytail: the whole file is read into memory to be scanned, once, and only for a file
    // whose xref is damaged.
    pub fn rebuilt(src: Arc<dyn Source>) -> Result<Pdf> {
        let scan = xref::scan(&src.read_vec(0, src.size() as usize)?);
        let xref = Xref {
            entries: scan.entries,
            trailer: scan.trailer.unwrap_or_default(),
            startxref: 0,
            stream: false,
        };
        let mut pdf = Pdf::over(src, xref)?;
        let mut members = Vec::new();
        for stream in scan.objstms {
            let Ok(stm) = pdf.objstm(stream, 0) else {
                continue;
            };
            for (index, num) in (0..).zip(stm.numbers()) {
                // An object of the file's own is newer than one packed with others before it.
                let entry = Entry::InStream { stream, index };
                if let std::collections::hash_map::Entry::Vacant(slot) = pdf.xref.entries.entry(num)
                {
                    slot.insert(entry);
                    members.push(Ref { num, generation: 0 });
                }
            }
        }
        // A file cut short has lost its trailer: its last catalog stands in for it.
        if pdf.trailer().get(b"Root").is_none() {
            let catalog = |r: &Ref| {
                let object = pdf.get(*r).ok()?;
                let kind = object.as_dict()?.get(b"Type")?.as_name()?;
                (kind == b"Catalog").then_some(*r)
            };
            let root =
                (scan.catalogs.last().copied()).or_else(|| members.iter().rev().find_map(catalog));
            if let Some(root) = root {
                pdf.xref.trailer.set(b"Root", Object::Ref(root));
            }
        }
        Ok(pdf)
    }

    /// The file `src` over the xref read of it, its security handler set up.
    fn over(src: Arc<dyn Source>, xref: Xref) -> Result<Pdf> {
        let encrypt = xref.trailer.get(b"Encrypt").cloned();
        let mut pdf = Pdf {
            src,
            xref,
            objstms: Mutex::default(),
            crypt: None,
            encryption: encrypt.as_ref().and_then(Object::reference),
        };
        if let Some(encrypt) = encrypt {
            let dict = pdf.resolve(Some(&encrypt))?;
            let id = match pdf.trailer().get(b"ID").and_then(Object::as_array) {
                Some([Object::String(id), ..]) => id.clone(),
                _ => Vec::new(),
            };
            let Some(dict) = dict.as_dict() else {
                return Err(Error::Broken("/Encrypt is no dictionary".into()));
            };
            pdf.crypt = Some(Crypt::new(dict, &id)?);
        }
        Ok(pdf)
    }

    /// The newest trailer.
    pub fn trailer(&self) -> &Dict {
        &self.xref.trailer
    }

    /// The version the header declares, `%PDF-1.0` to `%PDF-2.0`, within the first 1024 bytes as
    /// pdfium looks for it.
    pub fn version(&self) -> Option<String> {
        let head = self.src.read_vec(0, 1024).ok()?;
        let at = memchr::memmem::find(&head, b"%PDF-")? + 5;
        known_version(head.get(at..at + 3)?)
    }

    /// Where the newest xref section starts.
    pub fn startxref(&self) -> u64 {
        self.xref.startxref
    }

    /// Whether the newest xref section is a stream rather than a table.
    pub fn xref_is_stream(&self) -> bool {
        self.xref.stream
    }

    /// The object `r` names; null for a free or missing one, as the spec reads it. An object
    /// that is not where the xref puts it is [`Error::Broken`].
    pub fn get(&self, r: Ref) -> Result<Object> {
        self.get_in(r, 0)
    }

    fn get_in(&self, r: Ref, depth: usize) -> Result<Object> {
        if depth > DEPTH {
            return Err(Error::Broken(format!("object {} leads to itself", r.num)));
        }
        match self.xref.entries.get(&r.num) {
            Some(&Entry::InFile { offset, generation }) if generation == r.generation => {
                self.read_object(r, offset, depth)
            }
            Some(&Entry::InStream { stream, index }) if r.generation == 0 => {
                self.objstm(stream, depth)?.get(index as usize, r.num)
            }
            _ => Ok(Object::Null),
        }
    }

    /// The catalog.
    pub fn root(&self) -> Result<Dict> {
        match self.resolve(self.trailer().get(b"Root"))? {
            Object::Dict(d) => Ok(d),
            _ => Err(Error::Broken("no catalog".into())),
        }
    }

    /// Every page in order, at most the root's `/Count` of them.
    pub fn pages(&self) -> Result<Vec<Ref>> {
        let Some(top) = self.root()?.get(b"Pages").and_then(Object::reference) else {
            return Err(Error::Broken("no page tree".into()));
        };
        let count = self.resolve(self.get(top)?.as_dict().and_then(|d| d.get(b"Count")))?;
        let count = count
            .as_int()
            .and_then(|c| usize::try_from(c).ok())
            .unwrap_or(usize::MAX);
        let (mut pages, mut seen, mut stack) = (Vec::new(), HashSet::new(), vec![top]);
        while let Some(node) = stack.pop() {
            if pages.len() == count {
                break;
            }
            if !seen.insert(node) {
                return Err(Error::Broken("the page tree loops".into()));
            }
            let object = self.get(node)?;
            let Some(dict) = object.as_dict() else {
                continue;
            };
            let kind = dict.get(b"Type").and_then(Object::as_name);
            match (kind, self.resolve(dict.get(b"Kids"))?) {
                (Some(b"Page"), _) => pages.push(node),
                (_, Object::Array(kids)) => {
                    stack.extend(kids.iter().rev().filter_map(Object::reference));
                }
                (Some(b"Pages"), _) => {}
                _ => pages.push(node),
            }
        }
        Ok(pages)
    }

    /// `key` of `page`, or of the nearest node above it in the page tree that has it: the
    /// attributes a page inherits (`/Resources`, `/MediaBox`, `/CropBox`, `/Rotate`).
    pub fn inherited(&self, page: Ref, key: &[u8]) -> Result<Option<Object>> {
        let (mut node, mut seen) = (page, HashSet::new());
        while seen.insert(node) {
            let object = self.get(node)?;
            let Some(dict) = object.as_dict() else {
                return Ok(None);
            };
            if let Some(value) = dict.get(key) {
                return Ok(Some(value.clone()));
            }
            match dict.get(b"Parent").and_then(Object::reference) {
                Some(parent) => node = parent,
                None => return Ok(None),
            }
        }
        Err(Error::Broken("the page tree loops".into()))
    }

    /// A stream's data through its filters.
    pub fn decode(&self, stream: &Stream) -> Result<Vec<u8>> {
        self.decode_in(stream, 0)
    }

    fn decode_in(&self, stream: &Stream, depth: usize) -> Result<Vec<u8>> {
        let filter = self.resolve_in(stream.dict.get(b"Filter"), depth)?;
        let parms = self.resolve_in(stream.dict.get(b"DecodeParms"), depth)?;
        crate::filter::decode(&stream.raw, Some(&filter), Some(&parms))
    }

    /// `object`, or what it refers to; null for none.
    fn resolve(&self, object: Option<&Object>) -> Result<Object> {
        self.resolve_in(object, 0)
    }

    fn resolve_in(&self, object: Option<&Object>, depth: usize) -> Result<Object> {
        match object {
            Some(&Object::Ref(r)) => self.get_in(r, depth + 1),
            Some(o) => Ok(o.clone()),
            None => Ok(Object::Null),
        }
    }

    /// The object at `offset`, which must be the one `r` names, its stream data read.
    fn read_object(&self, r: Ref, offset: u64, depth: usize) -> Result<Object> {
        let (found, object, data) = xref::parse_at(&*self.src, offset, |lex| lex.indirect())?;
        if found != r {
            return Err(Error::Broken(format!(
                "object {} {} is not at its xref offset",
                r.num, r.generation
            )));
        }
        let mut object = match (object, data) {
            (Object::Stream(mut stream), Some(data)) => {
                stream.raw = self.stream_data(&stream.dict, offset + data as u64, depth)?;
                Object::Stream(stream)
            }
            (object, _) => object,
        };
        // An object stream's members come out of it decrypted already.
        if let Some(crypt) = &self.crypt
            && self.encryption != Some(r)
        {
            crypt.decrypt(r, &mut object);
        }
        Ok(object)
    }

    /// A stream's data from `start`: `/Length` bytes when `endstream` follows them, otherwise
    /// everything up to the keyword, as pdfium reads a stream whose length is wrong.
    fn stream_data(&self, dict: &Dict, start: u64, depth: usize) -> Result<Vec<u8>> {
        let length = match dict.get(b"Length") {
            Some(&Object::Ref(r)) => self.get_in(r, depth + 1)?.as_int(),
            Some(o) => o.as_int(),
            None => None,
        };
        let rest = self.src.size().saturating_sub(start);
        if let Some(n) = length
            .and_then(|n| u64::try_from(n).ok())
            .filter(|&n| n <= rest)
        {
            let mut data = self.src.read_vec(start, n as usize + 32)?;
            let after = data.get(n as usize..).unwrap_or_default();
            let white = after
                .iter()
                .take_while(|b| b.is_ascii_whitespace() || **b == 0)
                .count();
            if after[white..].starts_with(b"endstream") {
                data.truncate(n as usize);
                return Ok(data);
            }
        }
        let mut size = 4096;
        loop {
            let data = self.src.read_vec(start, size)?;
            if let Some(end) = memchr::memmem::find(&data, b"endstream") {
                let mut data = data[..end].to_vec();
                if data.ends_with(b"\r\n") {
                    data.truncate(end - 2);
                } else if data.ends_with(b"\n") || data.ends_with(b"\r") {
                    data.truncate(end - 1);
                }
                return Ok(data);
            }
            if (data.len() as u64) < size as u64 {
                return Err(Error::Broken("a stream without endstream".into()));
            }
            size *= 2;
        }
    }

    /// Object stream `num`, decoded once and kept while it is among the last read.
    fn objstm(&self, num: u32, depth: usize) -> Result<Arc<ObjStm>> {
        let cache = || self.objstms.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(stm) = cache().get(num) {
            return Ok(stm);
        }
        let Object::Stream(stream) = self.get_in(Ref { num, generation: 0 }, depth + 1)? else {
            return Err(Error::Broken(format!("object stream {num} is no stream")));
        };
        let data = self.decode_in(&stream, depth + 1)?;
        let stm = Arc::new(ObjStm::new(&stream.dict, data)?);
        cache().insert(num, stm.clone());
        Ok(stm)
    }
}

/// What `read` makes of the file `src`, through its own xref, or through one rebuilt from its
/// objects where that is damaged ([`Pdf::rebuilt`]): for whatever only reads a file.
pub fn read<T>(src: Arc<dyn Source>, read: impl Fn(&Pdf) -> Result<T>) -> Result<T> {
    match Pdf::open(src.clone()).and_then(|pdf| read(&pdf)) {
        Err(Error::Broken(_) | Error::Syntax { .. }) => read(&Pdf::rebuilt(src)?),
        done => done,
    }
}

/// `1.0` to `1.7`, or `2.0`: the versions there are.
pub(crate) fn known_version(v: &[u8]) -> Option<String> {
    match v {
        [major @ b'1', b'.', minor @ b'0'..=b'7'] | [major @ b'2', b'.', minor @ b'0'] => {
            Some(format!("{}.{}", char::from(*major), char::from(*minor)))
        }
        _ => None,
    }
}
