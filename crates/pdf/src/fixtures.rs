//! Documents for tests, here and in `accent-core` (the `fixtures` feature): encrypted ones, AES-128
//! (revision 4) and AES-256 (revision 6) under the empty user password, which nothing on the
//! machine writes (Ghostscript stops at RC4, revision 3; its files are `fixtures/*.pdf`), and a
//! form with a field of every kind.

use md5::{Digest, Md5};

use crate::crypt::{Crypt, PAD, aes, file_key, hash, rc4, user_entry};
use crate::{Dict, Object, Ref, write};

/// What the owner's password is.
const OWNER: &[u8] = b"owner";

/// The permissions of `/P`: everything but what bits 1–2 must leave clear.
const P: i32 = -4;

/// A one-page document under the standard security handler's revision `r`, 4 or 6, its user
/// password empty: a page of 200 × 100 points with "Hello" on it in Helvetica, and an `/Info`
/// whose `/Producer` is "accent fixtures".
pub fn encrypted_document(r: i64) -> Vec<u8> {
    let id = b"accent-fixture-0".to_vec();
    let encrypt = match r {
        4 => revision_4(&id),
        _ => revision_6(),
    };
    let text = |s: &str| crate::lex::Lexer::new(s.as_bytes(), 0, true).object().ok();
    let content = b"BT /F1 24 Tf 20 40 Td (Hello) Tj ET".to_vec();
    let mut objects = vec![
        text("<</Type/Catalog/Pages 2 0 R>>"),
        text("<</Type/Pages/Kids[3 0 R]/Count 1>>"),
        text("<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>"),
        Some(Object::Stream(crate::Stream { dict: Dict::default(), raw: content })),
        text("<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>"),
        text("<</Producer(accent fixtures)>>"),
    ]
    .into_iter()
    .map(|o| o.expect("written right"))
    .collect::<Vec<_>>();
    let crypt = Crypt::new(&encrypt, &id).expect("the user password is empty");
    for (i, object) in objects.iter_mut().enumerate() {
        crypt.encrypt(reference(i as u32 + 1), object);
    }
    objects.push(Object::Dict(encrypt));

    let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut rows = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        let r = reference(i as u32 + 1);
        rows.push((r, out.len() as u64));
        write::indirect(&mut out, r, object);
    }
    let xref = out.len();
    write::table(&mut out, &rows, true);
    let trailer = Object::Dict(Dict(vec![
        (b"Size".to_vec(), Object::Int(8)),
        (b"Root".to_vec(), Object::Ref(reference(1))),
        (b"Info".to_vec(), Object::Ref(reference(6))),
        (b"Encrypt".to_vec(), Object::Ref(reference(7))),
        (
            b"ID".to_vec(),
            Object::Array(vec![Object::String(id.clone()), Object::String(id)]),
        ),
    ]));
    out.extend(b"trailer\n");
    write::object(&mut out, &trailer);
    out.extend(format!("\nstartxref\n{xref}\n%%EOF\n").bytes());
    out
}

/// The objects of a form of two 400 × 300 pt pages, numbered from 1, the catalog first: a field
/// of every kind, which accent-core's tests, the generated vault's `Attachments/form.pdf` and the
/// form drill read. A text field merged with its widget, with a tooltip; a multi-line one shown
/// on both pages through two kids; one under a parent it inherits `/FT` and `/DA` from,
/// right-aligned; a check box whose on-state draws a red cross; a radio group across both pages
/// whose on-state draws a green square; a comb; a read-only field; a combo box of
/// `[export shown]` pairs; a multi-select list box with `/I`; a password; a Reset push button and
/// a signature.
pub fn form_objects() -> Vec<String> {
    let stream = |dict: &str, content: &str| {
        format!(
            "<<{dict}/Length {}>>stream\n{content}\nendstream",
            content.len()
        )
    };
    let widget =
        |page: u32, rest: &str| format!("<</Type/Annot/Subtype/Widget/F 4/P {page} 0 R{rest}>>");
    [
        "<</Type/Catalog/Pages 2 0 R/AcroForm 3 0 R>>".to_string(),
        "<</Type/Pages/Kids[4 0 R 5 0 R]/Count 2>>".to_string(),
        "<</Fields[9 0 R 10 0 R 13 0 R 15 0 R 18 0 R 23 0 R 24 0 R 25 0 R 26 0 R 27 0 R 28 0 R \
         29 0 R]/DA(/Helv 0 Tf 0 g)/DR<</Font<</Helv 6 0 R/ZaDb 7 0 R>>>>>>"
            .to_string(),
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 400 300]/Contents 8 0 R\
         /Resources<</Font<</F1 6 0 R>>>>/Annots[9 0 R 11 0 R 14 0 R 15 0 R 19 0 R 20 0 R \
         23 0 R 24 0 R 25 0 R 26 0 R 27 0 R 28 0 R 29 0 R]>>"
            .to_string(),
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 400 300]/Annots[12 0 R 21 0 R]>>".to_string(),
        "<</Type/Font/Subtype/Type1/BaseFont/Helvetica/Encoding/WinAnsiEncoding>>".to_string(),
        "<</Type/Font/Subtype/Type1/BaseFont/ZapfDingbats>>".to_string(),
        stream("", "BT /F1 10 Tf 20 280 Td (A form) Tj ET"),
        widget(
            4,
            "/FT/Tx/T(name)/TU(Your name)/V(Ada)/DA(/Helv 12 Tf 0 0 1 rg)/Rect[20 240 200 260]\
             /MK<</BG[1 1 0.9]/BC[0 0 0]>>",
        ),
        "<</FT/Tx/Ff 4096/T(address)/Kids[11 0 R 12 0 R]>>".to_string(),
        widget(4, "/Parent 10 0 R/Rect[20 180 200 230]"),
        widget(5, "/Parent 10 0 R/Rect[20 180 200 230]"),
        "<</T(person)/FT/Tx/DA(/Helv 10 Tf 0 g)/Kids[14 0 R]>>".to_string(),
        widget(4, "/Parent 13 0 R/T(email)/Q 2/Rect[220 240 380 260]"),
        widget(
            4,
            "/FT/Btn/T(agree)/V/Off/AS/Off/Rect[20 140 36 156]/AP<</N<</Yes 16 0 R/Off 17 0 R>>>>\
             /MK<</CA(4)>>/DA(/ZaDb 0 Tf 0 g)",
        ),
        stream(
            "/Type/XObject/Subtype/Form/BBox[0 0 16 16]",
            "1 0 0 RG 2 w 2 2 m 14 14 l 2 14 m 14 2 l S",
        ),
        stream("/Type/XObject/Subtype/Form/BBox[0 0 16 16]", ""),
        "<</FT/Btn/Ff 49152/T(size)/V/M/Kids[19 0 R 20 0 R 21 0 R]>>".to_string(),
        widget(
            4,
            "/Parent 18 0 R/Rect[60 140 76 156]/AS/Off/AP<</N<</S 22 0 R/Off 17 0 R>>>>",
        ),
        widget(
            4,
            "/Parent 18 0 R/Rect[90 140 106 156]/AS/M/AP<</N<</M 22 0 R/Off 17 0 R>>>>",
        ),
        widget(
            5,
            "/Parent 18 0 R/Rect[60 140 76 156]/AS/Off/AP<</N<</L 22 0 R/Off 17 0 R>>>>",
        ),
        stream(
            "/Type/XObject/Subtype/Form/BBox[0 0 16 16]",
            "0 0.6 0 rg 4 4 8 8 re f",
        ),
        widget(
            4,
            "/FT/Tx/T(code)/Ff 16777216/MaxLen 5/Rect[220 140 320 160]",
        ),
        widget(4, "/FT/Tx/T(amount)/Ff 1/V(12.50)/Rect[220 100 320 120]"),
        widget(
            4,
            "/FT/Ch/Ff 131072/T(country)/Opt[[(de)(Germany)][(fr)(France)][(it)(Italy)]]/V(fr)\
             /Rect[20 100 200 120]",
        ),
        widget(
            4,
            "/FT/Ch/Ff 2097152/T(fruit)/Opt[(Apple)(Banana)(Cherry)]/V[(Apple)(Cherry)]/I[0 2]\
             /Rect[20 30 120 90]",
        ),
        widget(4, "/FT/Tx/Ff 8192/T(secret)/Rect[220 60 320 80]"),
        widget(
            4,
            "/FT/Btn/Ff 65536/T(reset)/Rect[220 30 300 50]/A<</S/ResetForm>>/MK<</CA(Reset)>>",
        ),
        widget(4, "/FT/Sig/T(sign)/Rect[320 30 390 50]"),
    ]
    .into()
}

/// The form of [`form_objects`] as a file.
pub fn form_document() -> Vec<u8> {
    document_of(&form_objects())
}

/// A file of `objects`, numbered from 1, the first the catalog, in a classic xref table.
pub fn document_of(objects: &[String]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut rows = Vec::new();
    for (i, text) in objects.iter().enumerate() {
        let r = reference(i as u32 + 1);
        rows.push((r, out.len() as u64));
        out.extend(format!("{} 0 obj\n{text}\nendobj\n", r.num).bytes());
    }
    let xref = out.len();
    write::table(&mut out, &rows, true);
    let size = objects.len() + 1;
    let tail = format!("trailer\n<</Size {size}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n");
    out.extend(tail.bytes());
    out
}

fn reference(num: u32) -> Ref {
    Ref { num, generation: 0 }
}

fn name(n: &[u8]) -> Object {
    Object::Name(n.to_vec())
}

/// The standard handler's dictionary with one crypt filter, `/StdCF`, of method `cfm`.
fn handler(v: i64, r: i64, bits: i64, cfm: &[u8], keys: Vec<(&[u8], Vec<u8>)>) -> Dict {
    let filter = Dict(vec![
        (b"CFM".to_vec(), name(cfm)),
        (b"AuthEvent".to_vec(), name(b"DocOpen")),
        (b"Length".to_vec(), Object::Int(bits / 8)),
    ]);
    let mut dict = Dict(vec![
        (b"Filter".to_vec(), name(b"Standard")),
        (b"V".to_vec(), Object::Int(v)),
        (b"R".to_vec(), Object::Int(r)),
        (b"Length".to_vec(), Object::Int(bits)),
        (b"P".to_vec(), Object::Int(i64::from(P))),
        (
            b"CF".to_vec(),
            Object::Dict(Dict(vec![(b"StdCF".to_vec(), Object::Dict(filter))])),
        ),
        (b"StmF".to_vec(), name(b"StdCF")),
        (b"StrF".to_vec(), name(b"StdCF")),
    ]);
    for (key, value) in keys {
        dict.0.push((key.to_vec(), Object::String(value)));
    }
    dict
}

/// Revision 4, AES-128: `/O` from the owner's password (Algorithm 3), `/U` from the file key
/// (Algorithm 5), its second half anything.
fn revision_4(id: &[u8]) -> Dict {
    let padded = [OWNER, &PAD[..32 - OWNER.len()]].concat();
    let mut k = Md5::digest(&padded).to_vec();
    for _ in 0..50 {
        k = Md5::digest(&k).to_vec();
    }
    let mut o = PAD.to_vec();
    for i in 0..20u8 {
        let round: Vec<u8> = k.iter().map(|b| b ^ i).collect();
        o = rc4(&round, &o);
    }
    let key = file_key(4, 16, &o, P as u32, id, true);
    let u = [user_entry(4, &key, id), vec![0; 16]].concat();
    handler(4, 4, 128, b"AESV2", vec![(b"O", o), (b"U", u)])
}

/// Revision 6, AES-256 (Algorithms 8–10): a file key of its own, wrapped for each password,
/// and `/Perms` sealing the permissions under it.
fn revision_6() -> Dict {
    let key: Vec<u8> = (0..32).collect();
    let salts: Vec<u8> = (100..132).collect();
    let (uv, uk, ov, ok) = (&salts[..8], &salts[8..16], &salts[16..24], &salts[24..]);
    let u = [hash(6, b"", uv, b""), uv.to_vec(), uk.to_vec()].concat();
    let ue = aes(&hash(6, b"", uk, b""), &[0; 16], &key, true, false);
    let o = [hash(6, OWNER, ov, &u), ov.to_vec(), ok.to_vec()].concat();
    let oe = aes(&hash(6, OWNER, ok, &u), &[0; 16], &key, true, false);
    let perms = [&P.to_le_bytes()[..], &[0xff; 4], b"Tadb", &[0; 4]].concat();
    let perms = aes(&key, &[0; 16], &perms, true, false);
    let keys = vec![
        (b"O".as_slice(), o),
        (b"U", u),
        (b"OE", oe),
        (b"UE", ue),
        (b"Perms", perms),
    ];
    handler(5, 6, 256, b"AESV3", keys)
}
