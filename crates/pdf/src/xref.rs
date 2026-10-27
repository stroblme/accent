//! The cross-reference sections (ISO 32000-1 §7.5.4, §7.5.8): classic tables, xref streams and
//! hybrid files, followed from the newest back through `/Prev`, the newest entry of an object
//! winning.

use std::collections::{HashMap, HashSet};

use crate::lex::{Lexed, Lexer, Stop};
use crate::{Dict, Error, Object, Result, Source};

/// Where an object is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Entry {
    InFile {
        offset: u64,
        generation: u16,
    },
    /// The `index`th object of object stream `stream`.
    InStream {
        stream: u32,
        index: u32,
    },
    Free,
}

pub(crate) struct Xref {
    pub entries: HashMap<u32, Entry>,
    /// The newest section's trailer, an xref stream's dictionary for one.
    pub trailer: Dict,
    /// Where the newest section starts, which an update names as its `/Prev`.
    pub startxref: u64,
    /// Whether the newest section is an xref stream rather than a table.
    pub stream: bool,
}

/// What `parse` makes of the file from `at`, in a window of it that doubles until the object fits.
pub(crate) fn parse_at<T>(
    src: &dyn Source,
    at: u64,
    mut parse: impl FnMut(&mut Lexer) -> Lexed<T>,
) -> Result<T> {
    let len = src.size();
    let mut size = 4096;
    loop {
        let window = src.read_vec(at, size)?;
        let complete = at + window.len() as u64 >= len;
        match parse(&mut Lexer::new(&window, 0, complete)) {
            Ok(t) => return Ok(t),
            Err(Stop::End) if !complete => size *= 2,
            Err(Stop::End) => return Err(syntax(len, "unexpected end")),
            Err(Stop::Bad(pos, what)) => return Err(syntax(at + pos as u64, what)),
        }
    }
}

fn syntax(at: u64, what: &str) -> Error {
    Error::Syntax {
        at,
        what: what.to_string(),
    }
}

/// Every section of the file, from the one `startxref` names. One that does not parse is
/// [`Error::Broken`]: the offsets are not the file's, which only a repair can put right.
pub(crate) fn read(src: &dyn Source) -> Result<Xref> {
    read_sections(src).map_err(|e| match e {
        Error::Syntax { at, what } => Error::Broken(format!("xref at byte {at}: {what}")),
        e => e,
    })
}

fn read_sections(src: &dyn Source) -> Result<Xref> {
    let startxref = startxref(src)?;
    let mut entries = HashMap::new();
    let mut newest = None;
    let mut seen = HashSet::new();
    let mut next = Some(startxref);
    while let Some(at) = next {
        if !seen.insert(at) {
            return Err(Error::Broken("the xref's /Prev chain loops".into()));
        }
        let section = section(src, at)?;
        let mut revision: HashMap<u32, Entry> = HashMap::new();
        for (num, entry) in section.entries {
            revision.entry(num).or_insert(entry);
        }
        // A hybrid file's hidden xref stream: what its table leaves out or frees for older readers.
        if let Some(stm) = section.dict.get(b"XRefStm").and_then(Object::as_int) {
            let hidden = section_at(src, stm)?;
            for (num, entry) in hidden.entries {
                let slot = revision.entry(num).or_insert(entry);
                if *slot == Entry::Free {
                    *slot = entry;
                }
            }
        }
        for (num, entry) in revision {
            entries.entry(num).or_insert(entry);
        }
        next = section
            .dict
            .get(b"Prev")
            .and_then(Object::as_int)
            .map(|p| p as u64);
        if newest.is_none() {
            newest = Some((section.dict, section.stream));
        }
    }
    let (trailer, stream) = newest.expect("the chain starts with one section");
    Ok(Xref {
        entries,
        trailer,
        startxref,
        stream,
    })
}

/// The offset after the last `startxref`, which sits in a file's last bytes.
fn startxref(src: &dyn Source) -> Result<u64> {
    let len = src.size();
    let from = len.saturating_sub(4096);
    let tail = src.read_vec(from, 4096)?;
    let Some(at) = memchr::memmem::rfind(&tail, b"startxref") else {
        return Err(Error::Broken("no startxref".into()));
    };
    let mut lex = Lexer::new(&tail, at + b"startxref".len(), true);
    match lex.unsigned() {
        Ok(offset) if offset < len => Ok(offset),
        _ => Err(Error::Broken(
            "startxref names no offset in the file".into(),
        )),
    }
}

struct Section {
    entries: Vec<(u32, Entry)>,
    dict: Dict,
    stream: bool,
}

fn section_at(src: &dyn Source, at: i64) -> Result<Section> {
    let at = u64::try_from(at).map_err(|_| Error::Broken("negative xref offset".into()))?;
    section(src, at)
}

/// The section at `at`: a table and its trailer, or an xref stream.
fn section(src: &dyn Source, at: u64) -> Result<Section> {
    enum Head {
        Table(Vec<(u32, Entry)>, Dict),
        Stream(Dict, usize),
    }
    let head = parse_at(src, at, |lex| {
        let back = lex.pos;
        if lex.word()? == b"xref" {
            let (entries, dict) = table(lex)?;
            return Ok(Head::Table(entries, dict));
        }
        lex.pos = back;
        match lex.indirect()? {
            (_, Object::Stream(s), Some(data)) => Ok(Head::Stream(s.dict, data)),
            _ => Err(Stop::Bad(back, "no xref section")),
        }
    })?;
    let (dict, data) = match head {
        Head::Table(entries, dict) => {
            return Ok(Section {
                entries,
                dict,
                stream: false,
            });
        }
        Head::Stream(dict, data) => (dict, at + data as u64),
    };
    if dict.get(b"Type").and_then(Object::as_name) != Some(b"XRef") {
        return Err(Error::Broken("the xref offset holds no xref stream".into()));
    }
    let length = dict.get(b"Length").and_then(Object::as_int);
    let Some(length) = length.and_then(|l| usize::try_from(l).ok()) else {
        return Err(Error::Broken("xref stream without a direct /Length".into()));
    };
    let raw = src.read_vec(data, length)?;
    let data = crate::filter::decode(&raw, dict.get(b"Filter"), dict.get(b"DecodeParms"))?;
    let entries = stream_entries(&dict, &data)?;
    Ok(Section {
        entries,
        dict,
        stream: true,
    })
}

/// A table's subsections after `xref`, and the trailer dictionary after them. Lines of 19 or 21
/// bytes read as well as the 20 the spec asks for: entries are read as words.
fn table(lex: &mut Lexer) -> Lexed<(Vec<(u32, Entry)>, Dict)> {
    let mut entries = Vec::new();
    loop {
        let back = lex.pos;
        if lex.word()? == b"trailer" {
            break;
        }
        lex.pos = back;
        let (first, count) = (lex.unsigned()?, lex.unsigned()?);
        for num in first..first.saturating_add(count) {
            let at = lex.pos;
            let (offset, generation) = (lex.unsigned()?, lex.unsigned()?);
            // A generation past the 16 bits there are names no object: it reads as free.
            let entry = match (lex.word()?, u16::try_from(generation)) {
                (b"n", Ok(generation)) => Entry::InFile { offset, generation },
                (b"n" | b"f", _) => Entry::Free,
                _ => return Err(Stop::Bad(at, "xref entry")),
            };
            let num = u32::try_from(num).map_err(|_| Stop::Bad(at, "object number"))?;
            entries.push((num, entry));
        }
    }
    lex.skip_white();
    let at = lex.pos;
    match lex.object()? {
        Object::Dict(dict) => Ok((entries, dict)),
        _ => Err(Stop::Bad(at, "trailer")),
    }
}

/// An xref stream's entries: `/W` field widths over the `/Index` ranges.
fn stream_entries(dict: &Dict, data: &[u8]) -> Result<Vec<(u32, Entry)>> {
    let broken = |what: &str| Error::Broken(format!("xref stream {what}"));
    let ints = |key: &[u8]| -> Option<Vec<i64>> {
        dict.get(key)?
            .as_array()?
            .iter()
            .map(Object::as_int)
            .collect()
    };
    let w = ints(b"W").filter(|w| w.len() == 3 && w.iter().all(|&n| (0..=8).contains(&n)));
    let Some(w) = w.map(|w| w.iter().map(|&n| n as usize).collect::<Vec<_>>()) else {
        return Err(broken("/W"));
    };
    let size = dict.get(b"Size").and_then(Object::as_int).unwrap_or(0);
    let index = ints(b"Index").unwrap_or_else(|| vec![0, size]);
    if index.len() % 2 != 0 || w.iter().sum::<usize>() == 0 {
        return Err(broken("/Index"));
    }
    let field = |bytes: &[u8]| bytes.iter().fold(0u64, |v, &b| v << 8 | u64::from(b));
    let mut rows = data.chunks_exact(w.iter().sum());
    let mut entries = Vec::new();
    for range in index.chunks(2) {
        let (Ok(first), Ok(count)) = (u32::try_from(range[0]), u64::try_from(range[1])) else {
            return Err(broken("/Index"));
        };
        for i in 0..count {
            let (Ok(num), Some(row)) = (u32::try_from(u64::from(first) + i), rows.next()) else {
                return Ok(entries);
            };
            let (kind, rest) = row.split_at(w[0]);
            let (a, b) = rest.split_at(w[1]);
            // A missing type field means type 1.
            let kind = if w[0] == 0 { 1 } else { field(kind) };
            let entry = match kind {
                0 => Entry::Free,
                1 => Entry::InFile {
                    offset: field(a),
                    generation: field(b) as u16,
                },
                2 => Entry::InStream {
                    stream: field(a) as u32,
                    index: field(b) as u32,
                },
                // Unknown types are references to the null object.
                _ => continue,
            };
            entries.push((num, entry));
        }
    }
    Ok(entries)
}
