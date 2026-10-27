//! The reader against files built here, one per shape of xref: a table, an xref stream over
//! object streams, a hybrid, two revisions, and damaged ones.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::*;

/// A PDF written object by object, its xref sections added as the test asks.
struct Build {
    out: Vec<u8>,
    /// Objects written since the last section, and where.
    pending: BTreeMap<u32, u64>,
    /// The last section's offset, the next one's `/Prev`.
    last: Option<u64>,
}

impl Build {
    fn new() -> Build {
        Build {
            out: b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec(),
            pending: BTreeMap::new(),
            last: None,
        }
    }

    fn obj(&mut self, num: u32, body: &str) {
        self.pending.insert(num, self.out.len() as u64);
        self.out
            .extend(format!("{num} 0 obj\n{body}\nendobj\n").bytes());
    }

    fn stream(&mut self, num: u32, dict: &str, data: &[u8]) {
        self.pending.insert(num, self.out.len() as u64);
        let head = format!("{num} 0 obj\n<<{dict}/Length {}>>\nstream\n", data.len());
        self.out.extend(head.bytes());
        self.out.extend(data);
        self.out.extend(b"\nendstream\nendobj\n");
    }

    /// An object stream `num` holding `members`, Flate-compressed; their xref rows.
    fn objstm(&mut self, num: u32, members: &[(u32, &str)]) -> Vec<(u32, [u64; 3])> {
        let (mut head, mut body) = (String::new(), String::new());
        for (num, text) in members {
            head.push_str(&format!("{num} {} ", body.len()));
            body.push_str(text);
            body.push(' ');
        }
        let data =
            miniz_oxide::deflate::compress_to_vec_zlib(format!("{head}{body}").as_bytes(), 6);
        let dict = format!(
            "/Type/ObjStm/N {}/First {}/Filter/FlateDecode",
            members.len(),
            head.len()
        );
        self.stream(num, &dict, &data);
        (members.iter().enumerate())
            .map(|(i, (n, _))| (*n, [2, u64::from(num), i as u64]))
            .collect()
    }

    /// A table of the objects written since the last section, its trailer `extra` plus `/Prev`.
    fn table(&mut self, extra: &str) -> u64 {
        let at = self.out.len() as u64;
        let mut text = String::from("xref\n");
        if self.last.is_none() {
            text.push_str("0 1\n0000000000 65535 f \n");
        }
        for (num, offset) in std::mem::take(&mut self.pending) {
            text.push_str(&format!("{num} 1\n{offset:010} 00000 n \n"));
        }
        let prev = self.last.map_or(String::new(), |p| format!("/Prev {p}"));
        text.push_str(&format!("trailer\n<<{extra}{prev}>>\n"));
        self.out.extend(text.bytes());
        self.last = Some(at);
        at
    }

    /// An xref stream `num` of the objects written since the last section and of `rows`, each
    /// row PNG Up-predicted, as writers write them.
    fn xref_stream(&mut self, num: u32, rows: &[(u32, [u64; 3])], extra: &str) -> u64 {
        let at = self.out.len() as u64;
        self.pending.insert(num, at);
        let mut all: BTreeMap<u32, [u64; 3]> = rows.iter().copied().collect();
        for (n, offset) in std::mem::take(&mut self.pending) {
            all.insert(n, [1, offset, 0]);
        }
        let (mut index, mut data, mut prev) = (String::new(), Vec::new(), [0u8; 7]);
        for (n, [t, a, b]) in &all {
            index.push_str(&format!("{n} 1 "));
            let row = [
                &[*t as u8][..],
                &(*a as u32).to_be_bytes(),
                &(*b as u16).to_be_bytes(),
            ]
            .concat();
            data.push(2);
            data.extend(row.iter().zip(prev).map(|(r, p)| r.wrapping_sub(p)));
            prev.copy_from_slice(&row);
        }
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&data, 6);
        let prev = self.last.map_or(String::new(), |p| format!("/Prev {p}"));
        let dict = format!(
            "/Type/XRef/W[1 4 2]/Index[{index}]/Size {}/Filter/FlateDecode\
             /DecodeParms<</Predictor 12/Columns 7>>{extra}{prev}",
            all.keys().last().map_or(1, |n| n + 1)
        );
        let head = format!("{num} 0 obj\n<<{dict}/Length {}>>\nstream\n", z.len());
        self.out.extend(head.bytes());
        self.out.extend(&z);
        self.out.extend(b"\nendstream\nendobj\n");
        self.last = Some(at);
        at
    }

    fn end(&mut self, startxref: u64) -> Vec<u8> {
        self.out
            .extend(format!("startxref\n{startxref}\n%%EOF\n").bytes());
        self.out.clone()
    }
}

const CATALOG: &str = "<</Type/Catalog/Pages 2 0 R>>";
const TWO_PAGES: &str = "<</Type/Pages/Kids[3 0 R 4 0 R]/Count 2/MediaBox[0 0 200 100]>>";

fn page(text: &str) -> String {
    format!("<</Type/Page/Parent 2 0 R/Contents 5 0 R/Note({text})>>")
}

/// A two-page file in a classic table.
fn classic() -> Vec<u8> {
    let mut b = Build::new();
    b.obj(1, CATALOG);
    b.obj(2, TWO_PAGES);
    b.obj(3, &page("first"));
    b.obj(4, &page("second"));
    b.stream(5, "", b"0 0 m 10 10 l S");
    let at = b.table("/Size 6/Root 1 0 R");
    b.end(at)
}

fn open(bytes: Vec<u8>) -> Result<Pdf> {
    Pdf::open(Arc::new(bytes))
}

fn r(num: u32) -> Ref {
    Ref { num, generation: 0 }
}

fn note(pdf: &Pdf, num: u32) -> Vec<u8> {
    match pdf
        .get(r(num))
        .unwrap()
        .as_dict()
        .and_then(|d| d.get(b"Note"))
    {
        Some(Object::String(s)) => s.clone(),
        other => panic!("object {num} has no /Note: {other:?}"),
    }
}

#[test]
fn a_classic_table_reads_its_pages_and_streams() {
    let pdf = open(classic()).unwrap();
    assert!(!pdf.xref_is_stream());
    assert_eq!(pdf.pages().unwrap(), [r(3), r(4)]);
    assert_eq!(note(&pdf, 4), b"second");
    let Object::Stream(s) = pdf.get(r(5)).unwrap() else {
        panic!("no stream")
    };
    assert_eq!(pdf.decode(&s).unwrap(), b"0 0 m 10 10 l S");
    let media = pdf.inherited(r(3), b"MediaBox").unwrap();
    assert_eq!(
        media.and_then(|m| m.as_array().map(<[Object]>::len)),
        Some(4)
    );
    assert_eq!(pdf.get(r(99)).unwrap(), Object::Null);
}

/// The same two pages in an xref stream, their objects in an object stream.
fn compressed() -> (Vec<u8>, u64) {
    let mut b = Build::new();
    b.stream(5, "", b"0 0 m 10 10 l S");
    let rows = b.objstm(
        6,
        &[
            (1, CATALOG),
            (2, TWO_PAGES),
            (3, &page("first")),
            (4, &page("second")),
        ],
    );
    let at = b.xref_stream(7, &rows, "/Root 1 0 R");
    (b.end(at), at)
}

#[test]
fn an_xref_stream_finds_objects_in_object_streams() {
    let (bytes, at) = compressed();
    let pdf = open(bytes).unwrap();
    assert!(pdf.xref_is_stream());
    assert_eq!(pdf.startxref(), at);
    assert_eq!(pdf.pages().unwrap(), [r(3), r(4)]);
    assert_eq!(note(&pdf, 3), b"first");
}

#[test]
fn a_hybrid_file_reads_its_hidden_xref_stream() {
    let mut b = Build::new();
    b.obj(1, CATALOG);
    b.stream(5, "", b"0 0 m 10 10 l S");
    let rows = b.objstm(
        6,
        &[(2, TWO_PAGES), (3, &page("first")), (4, &page("second"))],
    );
    // The table older readers see lists what is in the file; the stream, what is compressed.
    let mut hidden = Build {
        out: Vec::new(),
        pending: BTreeMap::new(),
        last: None,
    };
    std::mem::swap(&mut hidden.out, &mut b.out);
    let stm = hidden.xref_stream(8, &rows, "");
    std::mem::swap(&mut hidden.out, &mut b.out);
    let at = b.table(&format!("/Size 9/Root 1 0 R/XRefStm {stm}"));
    let pdf = open(b.end(at)).unwrap();
    assert!(!pdf.xref_is_stream());
    assert_eq!(pdf.pages().unwrap(), [r(3), r(4)]);
    assert_eq!(note(&pdf, 4), b"second");
}

#[test]
fn the_newest_revision_wins() {
    let mut b = Build::new();
    b.obj(1, CATALOG);
    b.obj(2, TWO_PAGES);
    b.obj(3, &page("first"));
    b.obj(4, &page("second"));
    b.obj(6, "(soon gone)");
    let first = b.table("/Size 7/Root 1 0 R");
    b.out.extend(format!("startxref\n{first}\n%%EOF\n").bytes());
    b.obj(4, &page("second, edited"));
    let at = b.out.len() as u64;
    let section = format!(
        "xref\n4 1\n{:010} 00000 n \n6 1\n0000000000 00001 f \ntrailer\n<</Size 7/Root 1 0 R/Prev {first}>>\n",
        b.pending[&4]
    );
    b.out.extend(section.bytes());
    let pdf = open(b.end(at)).unwrap();
    assert_eq!(pdf.startxref(), at);
    assert_eq!(note(&pdf, 4), b"second, edited");
    assert_eq!(note(&pdf, 3), b"first");
    assert_eq!(
        pdf.get(r(6)).unwrap(),
        Object::Null,
        "freed in the newer revision"
    );
}

#[test]
fn an_object_not_where_the_xref_puts_it_is_broken() {
    let mut b = Build::new();
    b.obj(1, CATALOG);
    b.obj(2, TWO_PAGES);
    b.obj(3, &page("first"));
    b.obj(4, &page("second"));
    let four = b.pending[&4];
    b.pending.insert(3, four);
    let at = b.table("/Size 5/Root 1 0 R");
    let pdf = open(b.end(at)).unwrap();
    assert!(matches!(pdf.get(r(3)), Err(Error::Broken(_))));
}

#[test]
fn loops_are_errors_not_hangs() {
    // A section whose `/Prev` is itself.
    let mut b = Build::new();
    b.obj(1, CATALOG);
    let at = b.out.len() as u64;
    b.last = Some(at);
    b.table("/Size 2/Root 1 0 R");
    assert!(matches!(open(b.end(at)), Err(Error::Broken(_))));

    // A page tree whose kid is its own parent.
    let mut b = Build::new();
    b.obj(1, CATALOG);
    b.obj(2, "<</Type/Pages/Kids[3 0 R]/Count 5>>");
    b.obj(3, "<</Type/Pages/Kids[2 0 R]/Count 5>>");
    let at = b.table("/Size 4/Root 1 0 R");
    let pdf = open(b.end(at)).unwrap();
    assert!(matches!(pdf.pages(), Err(Error::Broken(_))));
    assert!(matches!(
        pdf.inherited(r(3), b"MediaBox"),
        Ok(None) | Err(_)
    ));
}

/// Whatever a damaged file holds, reading it ends: cut short at every 7th byte, or one byte
/// flipped at every 5th.
#[test]
fn damaged_files_never_panic() {
    let mut b = Build::new();
    b.stream(5, "", b"0 0 m 10 10 l S");
    let rows = b.objstm(
        6,
        &[
            (1, CATALOG),
            (2, TWO_PAGES),
            (3, &page("first")),
            (4, &page("second")),
        ],
    );
    let at = b.xref_stream(7, &rows, "/Root 1 0 R");
    for whole in [classic(), b.end(at)] {
        let read = |bytes: Vec<u8>| {
            if let Ok(pdf) = open(bytes) {
                let _ = pdf.pages();
                for num in 0..10 {
                    if let Ok(Object::Stream(s)) = pdf.get(r(num)) {
                        let _ = pdf.decode(&s);
                    }
                }
            }
        };
        for cut in (0..whole.len()).step_by(7) {
            read(whole[..cut].to_vec());
        }
        for at in (0..whole.len()).step_by(5) {
            let mut bytes = whole.to_vec();
            bytes[at] ^= 0x5a;
            read(bytes);
        }
    }
}

/// A session's section after `base`: in the base's own xref style, `/Prev` naming the base's
/// xref, the session's objects winning over the file's and new ones numbered past `/Size`; the
/// same section however often it is asked for.
fn round_trip(base: Vec<u8>, stream: bool) {
    let pdf = open(base.clone()).unwrap();
    let mut update = Update::new(&pdf);
    update.set(
        r(4),
        Object::Dict(Dict(vec![
            (b"Type".to_vec(), Object::Name(b"Page".to_vec())),
            (b"Parent".to_vec(), Object::Ref(r(2))),
            (
                b"Note".to_vec(),
                Object::String(b"edited (twice)\\".to_vec()),
            ),
        ])),
    );
    let data = Stream {
        dict: Dict(vec![(b"Length".to_vec(), Object::Int(999))]),
        raw: b"\x00\xffendstream\r\n".to_vec(),
    };
    let added = update.add(Object::Stream(data.clone()));
    let size = pdf.trailer().get(b"Size").and_then(Object::as_int).unwrap();
    assert_eq!(i64::from(added.num), size, "past the base's objects");
    assert_eq!(update.section(), update.section());

    let after = open([base, update.section()].concat()).unwrap();
    assert_eq!(after.xref_is_stream(), stream);
    let prev = after.trailer().get(b"Prev").and_then(Object::as_int);
    assert_eq!(prev, Some(pdf.startxref() as i64));
    assert_eq!(after.root().unwrap(), pdf.root().unwrap());
    assert_eq!(note(&after, 4), b"edited (twice)\\");
    assert_eq!(note(&after, 3), b"first");
    assert_eq!(after.pages().unwrap(), [r(3), r(4)]);
    let Object::Stream(read) = after.get(added).unwrap() else {
        panic!("no stream")
    };
    assert_eq!(read.raw, data.raw);
    assert_eq!(update.get(&pdf, r(4)).unwrap(), after.get(r(4)).unwrap());
    assert_eq!(update.get(&pdf, r(3)).unwrap(), after.get(r(3)).unwrap());
}

#[test]
fn a_section_after_a_table_round_trips() {
    round_trip(classic(), false);
}

#[test]
fn a_section_after_an_xref_stream_round_trips() {
    round_trip(compressed().0, true);
}

#[test]
fn a_new_document_is_one_page_of_its_size() {
    let pdf = open(new_document((595.0, 842.5))).unwrap();
    let pages = pdf.pages().unwrap();
    assert_eq!(pages.len(), 1);
    let media = pdf.inherited(pages[0], b"MediaBox").unwrap().unwrap();
    let media: Vec<f64> = (media.as_array().unwrap().iter())
        .map(|n| match n {
            Object::Int(i) => *i as f64,
            Object::Real(r) => *r,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(media, [0.0, 0.0, 595.0, 842.5]);
}
