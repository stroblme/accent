//! Jupyter's wire protocol: a message as the frames a socket carries, signed with HMAC-SHA256
//! under the connection file's key (messaging spec, "The Wire Protocol").

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;

use crate::Error;

/// What separates the routing frames, and IOPub's topic, from a message's own.
const DELIMITER: &[u8] = b"<IDS|MSG>";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Header {
    pub msg_id: String,
    pub session: String,
    pub username: String,
    pub date: String,
    pub msg_type: String,
    pub version: String,
}

impl Header {
    /// The header of our `n`th message in `session`, of `msg_type`.
    pub(crate) fn new(session: &str, n: u64, msg_type: &str) -> Header {
        Header {
            msg_id: format!("{session}_{n}"),
            session: session.to_string(),
            username: "accent".to_string(),
            date: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            msg_type: msg_type.to_string(),
            version: "5.3".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub header: Header,
    /// The message this one answers or was caused by; `{}` on the wire for none.
    pub parent: Option<Header>,
    pub metadata: Value,
    pub content: Value,
}

/// The key messages are signed with: the connection file's, as its UTF-8 bytes. An empty key
/// signs nothing, as Jupyter's does.
pub(crate) struct Signer(Vec<u8>);

impl Signer {
    pub(crate) fn new(key: &[u8]) -> Signer {
        Signer(key.to_vec())
    }

    fn mac(&self, parts: [&[u8]; 4]) -> Option<Hmac<Sha256>> {
        if self.0.is_empty() {
            return None;
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).expect("HMAC takes any key");
        for part in parts {
            mac.update(part);
        }
        Some(mac)
    }
}

/// `msg` as the frames a DEALER sends: the delimiter, the signature, then the four dicts.
pub(crate) fn encode(signer: &Signer, msg: &Message) -> Vec<Vec<u8>> {
    let dict = |value: &Value| match value {
        Value::Null => b"{}".to_vec(),
        value => serde_json::to_vec(value).expect("JSON serialises"),
    };
    let header = serde_json::to_vec(&msg.header).expect("a header serialises");
    let parent = match &msg.parent {
        Some(parent) => serde_json::to_vec(parent).expect("a header serialises"),
        None => b"{}".to_vec(),
    };
    let (metadata, content) = (dict(&msg.metadata), dict(&msg.content));
    let signature = signer
        .mac([&header, &parent, &metadata, &content])
        .map_or_else(String::new, |mac| hex(&mac.finalize().into_bytes()));
    let delimiter = DELIMITER.to_vec();
    vec![
        delimiter,
        signature.into_bytes(),
        header,
        parent,
        metadata,
        content,
    ]
}

/// The message `frames` hold, whatever routing frames or topic come before the delimiter; any
/// buffers after its four dicts are left out.
pub(crate) fn decode(signer: &Signer, frames: Vec<Vec<u8>>) -> Result<Message, Error> {
    let at = frames.iter().position(|f| f == DELIMITER);
    let Some([signature, header, parent, metadata, content]) =
        at.and_then(|at| frames.get(at + 1..at + 6))
    else {
        return Err(Error::Malformed("its frames are not a message's".into()));
    };
    if let Some(mac) = signer.mac([header, parent, metadata, content]) {
        let signed = unhex(signature).is_some_and(|s| mac.verify_slice(&s).is_ok());
        if !signed {
            return Err(Error::BadSignature);
        }
    }
    let read = |bytes: &[u8]| -> Result<Value, Error> {
        serde_json::from_slice(bytes).map_err(|e| Error::Malformed(e.to_string()))
    };
    let header = serde_json::from_slice(header).map_err(|e| Error::Malformed(e.to_string()))?;
    let parent = match read(parent)? {
        Value::Object(dict) if dict.is_empty() => None,
        Value::Null => None,
        parent => serde_json::from_value(parent).ok(),
    };
    Ok(Message {
        header,
        parent,
        metadata: read(metadata)?,
        content: read(content)?,
    })
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &[u8]) -> Option<Vec<u8>> {
    let digit = |c: u8| (c as char).to_digit(16);
    text.chunks(2)
        .map(|pair| match pair {
            [high, low] => Some((digit(*high)? * 16 + digit(*low)?) as u8),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_is_signed_as_jupyter_signs_it() {
        let signer = Signer::new(b"abc");
        let header = Header {
            msg_id: "s_1".into(),
            session: "s".into(),
            username: "accent".into(),
            date: "2026-10-20T00:00:00.000000Z".into(),
            msg_type: "kernel_info_request".into(),
            version: "5.3".into(),
        };
        let msg = Message {
            header,
            parent: None,
            metadata: Value::Null,
            content: serde_json::json!({}),
        };
        let frames = encode(&signer, &msg);
        // `hmac.new(b"abc", digestmod=hashlib.sha256)` over the four dicts, as jupyter_client's
        // `Session.sign` computes it.
        let signature = "41602cff09ff4366ca310e2e8c0830ce7696099949d701c7838e3a760a1741db";
        assert_eq!(frames[1], signature.as_bytes());
        assert_eq!(
            frames[3..],
            [b"{}".to_vec(), b"{}".to_vec(), b"{}".to_vec()]
        );

        // What comes before the delimiter is routing, and is skipped.
        let routed = |frames: &[Vec<u8>]| [&[b"identity".to_vec()], frames].concat();
        let read = decode(&signer, routed(&frames)).unwrap();
        assert_eq!((read.header, read.parent), (msg.header, None));
        let mut changed = frames.clone();
        changed[5] = b"{\"a\":1}".to_vec();
        assert!(matches!(decode(&signer, changed), Err(Error::BadSignature)));
    }
}
