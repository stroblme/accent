//! The standard security handler (ISO 32000-1 §7.6.3, ISO 32000-2 §7.6.4.3.4 for AES-256),
//! revisions 2–6, keyed by the empty user password: what opens a file whose only lock is its
//! owner's restrictions, as pdfium opens it. Strings and streams are decrypted as they are read
//! and encrypted as an update writes them; `/Encrypt`, xref streams and `/ID` never are.

use aes::cipher::block_padding::{NoPadding, Pkcs7};
use aes::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyInit, KeyIvInit, StreamCipher};
use aes::{Aes128, Aes256};
use md5::{Digest, Md5};
use sha2::{Sha256, Sha384, Sha512};

use crate::{Dict, Error, Object, Ref, Result};

/// What a password is padded with to 32 bytes; the empty password is this alone.
pub(crate) const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// How strings or streams are encrypted: a crypt filter's `/CFM`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Method {
    Identity,
    Rc4,
    Aes128,
    Aes256,
}

#[derive(Debug, Clone)]
pub(crate) struct Crypt {
    /// The file key.
    key: Vec<u8>,
    streams: Method,
    strings: Method,
    /// Whether the metadata stream is encrypted too (`/EncryptMetadata`).
    metadata: bool,
}

impl Crypt {
    /// The handler `dict` describes, for the file identified by `id` (the first of its `/ID`).
    /// A file that needs a password to open is [`Error::Unsupported`].
    pub fn new(dict: &Dict, id: &[u8]) -> Result<Crypt> {
        let int = |key: &[u8]| dict.get(key).and_then(Object::as_int);
        let bytes = |key: &[u8]| match dict.get(key) {
            Some(Object::String(s)) => s.as_slice(),
            _ => &[],
        };
        let filter = dict
            .get(b"Filter")
            .and_then(Object::as_name)
            .unwrap_or_default();
        if filter != b"Standard" {
            let name = String::from_utf8_lossy(filter);
            return Err(Error::Unsupported(format!("the {name} security handler")));
        }
        let (v, r) = (int(b"V").unwrap_or(0), int(b"R").unwrap_or(0));
        let metadata = dict.get(b"EncryptMetadata") != Some(&Object::Bool(false));
        let (streams, strings, len) = match v {
            1 => (Method::Rc4, Method::Rc4, 5),
            2 => {
                let bits = int(b"Length").unwrap_or(40).clamp(40, 128);
                (Method::Rc4, Method::Rc4, bits as usize / 8)
            }
            4 | 5 => {
                let method = |key: &[u8]| crypt_filter(dict, key);
                (
                    method(b"StmF")?,
                    method(b"StrF")?,
                    if v == 5 { 32 } else { 16 },
                )
            }
            _ => return Err(Error::Unsupported(format!("encryption version {v}"))),
        };
        let locked = || Error::Unsupported("a file that needs a password".into());
        let key = match r {
            2..=4 => {
                let p = int(b"P").unwrap_or(0) as u32;
                let key = file_key(r, len, bytes(b"O"), p, id, metadata);
                // All of `/U` before revision 3, its first half from then on.
                let n = if r == 2 { 32 } else { 16 };
                if bytes(b"U").get(..n) != user_entry(r, &key, id).get(..n) {
                    return Err(locked());
                }
                key
            }
            5 | 6 => {
                let (u, ue) = (bytes(b"U"), bytes(b"UE"));
                if u.len() < 48 || ue.len() < 32 {
                    return Err(Error::Broken("an AES-256 /U or /UE too short".into()));
                }
                if hash(r, b"", &u[32..40], b"")[..] != u[..32] {
                    return Err(locked());
                }
                aes(
                    &hash(r, b"", &u[40..48], b""),
                    &[0; 16],
                    &ue[..32],
                    false,
                    false,
                )
            }
            _ => return Err(Error::Unsupported(format!("security handler revision {r}"))),
        };
        Ok(Crypt {
            key,
            streams,
            strings,
            metadata,
        })
    }

    /// Every string of `object`, and a stream's data, decrypted as object `r`'s.
    pub fn decrypt(&self, r: Ref, object: &mut Object) {
        self.walk(r, object, false);
    }

    /// Every string of `object`, and a stream's data, encrypted as object `r`'s.
    pub fn encrypt(&self, r: Ref, object: &mut Object) {
        self.walk(r, object, true);
    }

    fn walk(&self, r: Ref, object: &mut Object, encrypt: bool) {
        match object {
            Object::String(s) => *s = self.apply(r, self.strings, s, encrypt),
            Object::Array(items) => items.iter_mut().for_each(|o| self.walk(r, o, encrypt)),
            Object::Dict(d) => d.0.iter_mut().for_each(|(_, o)| self.walk(r, o, encrypt)),
            Object::Stream(s) => {
                s.dict
                    .0
                    .iter_mut()
                    .for_each(|(_, o)| self.walk(r, o, encrypt));
                if self.encrypted(&s.dict) {
                    s.raw = self.apply(r, self.streams, &s.raw, encrypt);
                }
            }
            _ => {}
        }
    }

    /// Whether a stream's data is encrypted: not an xref stream's, not the metadata's where
    /// `/EncryptMetadata` says so, and not one with a `/Crypt` filter of its own, which in
    /// practice is `/Identity`.
    fn encrypted(&self, dict: &Dict) -> bool {
        let kind = dict.get(b"Type").and_then(Object::as_name);
        let crypt = |f: &Object| f.as_name() == Some(b"Crypt");
        let own = match dict.get(b"Filter") {
            Some(Object::Array(filters)) => filters.iter().any(crypt),
            Some(filter) => crypt(filter),
            None => false,
        };
        kind != Some(b"XRef") && !(kind == Some(b"Metadata") && !self.metadata) && !own
    }

    fn apply(&self, r: Ref, method: Method, data: &[u8], encrypt: bool) -> Vec<u8> {
        let key = match method {
            Method::Identity => return data.to_vec(),
            Method::Aes256 => self.key.clone(),
            _ => self.object_key(r, method == Method::Aes128),
        };
        match (method, encrypt) {
            (Method::Rc4, _) => rc4(&key, data),
            (_, true) => {
                let iv = fresh_iv();
                [&iv[..], &aes(&key, &iv, data, true, true)].concat()
            }
            (_, false) if data.len() < 16 => Vec::new(),
            (_, false) => aes(&key, &data[..16], &data[16..], false, true),
        }
    }

    /// Object `r`'s key (Algorithm 1): the file key with its number and generation, salted
    /// for AES.
    fn object_key(&self, r: Ref, salt: bool) -> Vec<u8> {
        let mut md5 = Md5::new();
        md5.update(&self.key);
        md5.update(&r.num.to_le_bytes()[..3]);
        md5.update(r.generation.to_le_bytes());
        if salt {
            md5.update(b"sAlT");
        }
        md5.finalize()[..(self.key.len() + 5).min(16)].to_vec()
    }
}

/// The method of the crypt filter `/StmF` or `/StrF` names; `/Identity` when it names none.
fn crypt_filter(dict: &Dict, key: &[u8]) -> Result<Method> {
    let name = dict
        .get(key)
        .and_then(Object::as_name)
        .unwrap_or(b"Identity");
    if name == b"Identity" {
        return Ok(Method::Identity);
    }
    let filters = dict.get(b"CF").and_then(Object::as_dict);
    let filter = filters.and_then(|f| f.get(name)).and_then(Object::as_dict);
    match filter.and_then(|f| f.get(b"CFM")).and_then(Object::as_name) {
        Some(b"V2") => Ok(Method::Rc4),
        Some(b"AESV2") => Ok(Method::Aes128),
        Some(b"AESV3") => Ok(Method::Aes256),
        Some(b"None") | None => Ok(Method::Identity),
        Some(other) => {
            let other = String::from_utf8_lossy(other);
            Err(Error::Unsupported(format!("the {other} crypt filter")))
        }
    }
}

/// The file key for the empty user password (Algorithm 2), `len` bytes.
pub(crate) fn file_key(r: i64, len: usize, o: &[u8], p: u32, id: &[u8], metadata: bool) -> Vec<u8> {
    let mut md5 = Md5::new_with_prefix(PAD);
    md5.update(o);
    md5.update(p.to_le_bytes());
    md5.update(id);
    if r >= 4 && !metadata {
        md5.update([0xff; 4]);
    }
    let mut key = md5.finalize().to_vec();
    if r >= 3 {
        for _ in 0..50 {
            key = Md5::digest(&key[..len]).to_vec();
        }
    }
    key.truncate(len);
    key
}

/// What `/U` holds for the empty user password under `key` (Algorithms 4 and 5).
pub(crate) fn user_entry(r: i64, key: &[u8], id: &[u8]) -> Vec<u8> {
    if r == 2 {
        return rc4(key, &PAD);
    }
    let mut u = Md5::new_with_prefix(PAD)
        .chain_update(id)
        .finalize()
        .to_vec();
    for i in 0..20u8 {
        let round: Vec<u8> = key.iter().map(|k| k ^ i).collect();
        u = rc4(&round, &u);
    }
    u
}

/// A revision 5 or 6 password hash (Algorithm 2.B) of `password` with `salt` and, for the
/// owner's, `udata`.
pub(crate) fn hash(r: i64, password: &[u8], salt: &[u8], udata: &[u8]) -> Vec<u8> {
    let mut k = Sha256::new_with_prefix(password)
        .chain_update(salt)
        .chain_update(udata)
        .finalize()
        .to_vec();
    if r == 5 {
        return k;
    }
    let mut round = 0u32;
    loop {
        let k1 = [password, &k[..], udata].concat().repeat(64);
        let e = aes(&k[..16], &k[16..32], &k1, true, false);
        // The first 16 bytes as a number, mod 3: the sum of the bytes mod 3, as 256 is 1 mod 3.
        k = match e[..16].iter().map(|&b| u32::from(b)).sum::<u32>() % 3 {
            0 => Sha256::digest(&e).to_vec(),
            1 => Sha384::digest(&e).to_vec(),
            _ => Sha512::digest(&e).to_vec(),
        };
        round += 1;
        if round >= 64 && u32::from(e[e.len() - 1]) <= round - 32 {
            break;
        }
    }
    k.truncate(32);
    k
}

pub(crate) fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut out = data.to_vec();
    rc4::Rc4::new_from_slice(key)
        .expect("keys are 5 to 32 bytes")
        .apply_keystream(&mut out);
    out
}

/// AES-CBC with a 16- or 32-byte `key`, `pad`ded as strings and streams are (PKCS #7) or not,
/// as the key material of revisions 5 and 6 is not. Data whose padding does not read is taken
/// whole rather than dropped, and a part block at the end is left off.
pub(crate) fn aes(key: &[u8], iv: &[u8], data: &[u8], encrypt: bool, pad: bool) -> Vec<u8> {
    let blocks = &data[..data.len() / 16 * 16];
    macro_rules! with {
        ($cipher:ty) => {{
            let raw = |d: &[u8]| -> Option<Vec<u8>> {
                cbc::Decryptor::<$cipher>::new_from_slices(key, iv)
                    .ok()?
                    .decrypt_padded_vec::<NoPadding>(d)
                    .ok()
            };
            let padded = |d: &[u8]| -> Option<Vec<u8>> {
                cbc::Decryptor::<$cipher>::new_from_slices(key, iv)
                    .ok()?
                    .decrypt_padded_vec::<Pkcs7>(d)
                    .ok()
            };
            let enc = cbc::Encryptor::<$cipher>::new_from_slices(key, iv).ok();
            match (encrypt, pad) {
                (true, true) => enc.map(|c| c.encrypt_padded_vec::<Pkcs7>(data)),
                (true, false) => enc.map(|c| c.encrypt_padded_vec::<NoPadding>(blocks)),
                (false, true) => padded(blocks).or_else(|| raw(blocks)),
                (false, false) => raw(blocks),
            }
            .unwrap_or_default()
        }};
    }
    match key.len() {
        16 => with!(Aes128),
        _ => with!(Aes256),
    }
}

/// A fresh initialisation vector: the hasher keys std seeds from the system's randomness. The
/// files this writes have no secret to keep (the user password is empty); the vector need only
/// differ.
fn fresh_iv() -> [u8; 16] {
    use std::hash::BuildHasher;
    let seed = std::collections::hash_map::RandomState::new();
    let mut iv = [0; 16];
    iv[..8].copy_from_slice(&seed.hash_one(1u8).to_le_bytes());
    iv[8..].copy_from_slice(&seed.hash_one(2u8).to_le_bytes());
    iv
}
