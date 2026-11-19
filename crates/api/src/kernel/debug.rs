//! A kernel's debugger as a debug adapter. ipykernel takes the Debug Adapter Protocol in
//! `debug_request`s on its control channel and answers each in a `debug_reply`, its debugpy's
//! events coming as `debug_event`s on IOPub (Jupyter messaging 5.5). The bridge carries them to
//! and from an [`accent_dap::Client`], so a notebook's session is an ordinary debug session.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use accent_dap::{Client, Incomings};
use accent_jupyter::{Channel, Incoming};
use accent_lsp::{codec, runtime};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::mpsc;

use super::session::Session;
use crate::locked;

/// The seed ipykernel hashes a cell's code with to name its file (`get_tmp_hash_seed`).
const SEED: u32 = 0xC70F_6907;

/// Each bridge's number, so the one a kernel holds is told from one it let go.
static BRIDGES: AtomicU64 = AtomicU64::new(0);

/// How a kernel names the file of each cell it runs, as its `debugInfo` says: the hash of the
/// cell's code between `prefix` and `suffix`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cells {
    pub prefix: String,
    pub suffix: String,
}

impl Cells {
    /// The file the kernel ran `code` as, which the frames of its calls name.
    pub fn path(&self, code: &str) -> String {
        let hash = murmur2(code.as_bytes(), SEED);
        format!("{}{hash}{}", self.prefix, self.suffix)
    }
}

/// ipykernel's `debugInfo` answer, as far as Accent reads it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct DebugInfo {
    hash_method: String,
    hash_seed: u32,
    tmp_file_prefix: String,
    tmp_file_suffix: String,
}

impl DebugInfo {
    /// How the cells are named, where they are hashed as [`Cells::path`] hashes them.
    pub(crate) fn cells(self) -> Option<Cells> {
        let ours = self.hash_method == "Murmur2" && self.hash_seed == SEED;
        ours.then_some(Cells {
            prefix: self.tmp_file_prefix,
            suffix: self.tmp_file_suffix,
        })
    }
}

/// ipykernel's `dumpCell` answer: the file it wrote the code to.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct DumpCell {
    pub(crate) source_path: String,
}

/// What a kernel's debugger is reached by: a client, its news, and the Python the kernel runs in,
/// which a missing debugpy is named by.
pub(crate) struct Debuggee {
    pub(crate) client: Client,
    pub(crate) incomings: Incomings,
    pub(crate) python: String,
}

/// The requests sent and not yet answered, by their `debug_request`'s id: the DAP `seq` and
/// command each answers.
type Pending = Arc<Mutex<HashMap<String, (i64, String)>>>;

/// A client of `session`'s debugger, which takes its place from any client before. It ends as the
/// kernel does, its news then ending; and the kernel lets go of it as it shuts down.
pub(super) fn connect(session: &Arc<Session>, python: String) -> Debuggee {
    let (ours, theirs) = tokio::io::duplex(64 * 1024);
    let (from_kernel, news) = mpsc::unbounded_channel();
    let id = BRIDGES.fetch_add(1, Ordering::Relaxed);
    locked(&session.debug).replace((id, from_kernel));
    let (read, write) = tokio::io::split(ours);
    let (client, incomings) = Client::new(read, write);
    let (read, write) = tokio::io::split(theirs);
    let pending = Pending::default();
    let weak = Arc::downgrade(session);
    runtime().spawn(requests(id, weak, read, pending.clone()));
    runtime().spawn(answers(news, write, pending));
    Debuggee {
        client,
        incomings,
        python,
    }
}

/// Send the client's requests to the kernel until either is gone, then let go of the kernel.
async fn requests(
    id: u64,
    session: Weak<Session>,
    reader: ReadHalf<DuplexStream>,
    pending: Pending,
) {
    let mut reader = BufReader::new(reader);
    while let Ok(Some(body)) = codec::read(&mut reader).await {
        let Ok(request) = serde_json::from_slice::<Value>(&body) else {
            continue;
        };
        let (Some(seq), Some(command)) = (request["seq"].as_i64(), request["command"].as_str())
        else {
            continue;
        };
        let command = command.to_string();
        let Some(session) = session.upgrade() else {
            break;
        };
        // Kept under the lock the answers are matched under, so none comes before it.
        let mut waiting = locked(&pending);
        match session
            .kernel
            .send(Channel::Control, "debug_request", request, None)
        {
            Ok(msg_id) => drop(waiting.insert(msg_id, (seq, command))),
            Err(e) => {
                tracing::debug!("the kernel took no debug request: {e}");
                break;
            }
        }
    }
    // The answers end with the kernel's hold on this bridge, unless another has taken it.
    if let Some(session) = session.upgrade() {
        let mut held = locked(&session.debug);
        if held.as_ref().is_some_and(|(held, _)| *held == id) {
            held.take();
        }
    }
}

/// Write the kernel's debug news to the client, each answer as the request it answers, until the
/// kernel lets go: the client then reads the end.
async fn answers(
    mut news: mpsc::UnboundedReceiver<Incoming>,
    mut writer: WriteHalf<DuplexStream>,
    pending: Pending,
) {
    while let Some(incoming) = news.recv().await {
        let message = match incoming {
            Incoming::Message(_, msg) if msg.header.msg_type == "debug_event" => msg.content,
            Incoming::Message(_, msg) => {
                let parent = msg.parent.as_ref().map(|p| p.msg_id.as_str());
                let Some((seq, command)) = parent.and_then(|p| locked(&pending).remove(p)) else {
                    continue;
                };
                envelope(&msg.content, seq, &command)
            }
            // An answer past what a message may hold, a huge value's: the request fails.
            Incoming::Dropped {
                channel: Channel::Control,
                parent: Some(parent),
                ..
            } => {
                let Some((seq, command)) = locked(&pending).remove(&parent) else {
                    continue;
                };
                let too_large = json!({"message": "the kernel's answer is too large to read"});
                envelope(&too_large, seq, &command)
            }
            _ => continue,
        };
        let Ok(body) = serde_json::to_vec(&message) else {
            continue;
        };
        if writer.write_all(&codec::frame(&body)).await.is_err() {
            break;
        }
    }
    let _ = writer.shutdown().await;
}

/// `content`, a `debug_reply`'s, as the DAP answer to request `seq`, `command`. ipykernel answers
/// `{}` where debugpy is missing, misnames `request_seq` in one answer and sends another bare, and
/// takes whatever debugpy answers next for the answer it waits for: one naming another command
/// fails rather than be read as this one's, whose body would read as an empty answer.
pub(crate) fn envelope(content: &Value, seq: i64, command: &str) -> Value {
    let said = content["command"].as_str();
    let ours = said.is_none_or(|said| said == command);
    let mut answer = json!({
        "type": "response",
        "request_seq": seq,
        "command": command,
        "success": ours && content["success"].as_bool().unwrap_or(false),
    });
    if !ours {
        answer["message"] =
            format!("the kernel answered {} in its place", said.unwrap_or("")).into();
        return answer;
    }
    for key in ["message", "body"] {
        if let Some(value) = content.get(key) {
            answer[key] = value.clone();
        }
    }
    answer
}

/// MurmurHash2's 32-bit hash of `data`, as ipykernel's `murmur2_x86` hashes a cell's UTF-8.
fn murmur2(data: &[u8], seed: u32) -> u32 {
    const M: u32 = 0x5BD1_E995;
    let mut h = seed ^ data.len() as u32;
    let (words, tail) = data.as_chunks::<4>();
    for word in words {
        let mut k = u32::from_le_bytes(*word);
        k = k.wrapping_mul(M);
        k ^= k >> 24;
        k = k.wrapping_mul(M);
        h = h.wrapping_mul(M) ^ k;
    }
    if !tail.is_empty() {
        let k = tail.iter().rev().fold(0, |k, &b| (k << 8) | u32::from(b));
        h = (h ^ k).wrapping_mul(M);
    }
    h ^= h >> 13;
    h = h.wrapping_mul(M);
    h ^ (h >> 15)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ipykernel 6.31's and 7.4's `murmur2_x86(code, 0xC70F6907)` for these.
    #[test]
    fn a_cell_is_named_as_ipykernel_names_it() {
        let named = [
            ("", 3990065800),
            ("x = 1\n", 1602534638),
            ("print(\"é\")", 2069358938),
            ("def double(x):\n    return x * 2\n", 1690760340),
        ];
        for (code, hash) in named {
            assert_eq!(murmur2(code.as_bytes(), SEED), hash, "{code:?}");
        }
        let cells = Cells {
            prefix: "/tmp/ipykernel_7/".to_string(),
            suffix: ".py".to_string(),
        };
        assert_eq!(cells.path("x = 1\n"), "/tmp/ipykernel_7/1602534638.py");
    }

    #[test]
    fn a_kernels_answers_read_as_the_requests_they_answer() {
        // debugpy missing: ipykernel answers nothing but an empty content.
        let missing = envelope(&json!({}), 3, "debugInfo");
        assert_eq!(
            missing,
            json!({"type": "response", "request_seq": 3, "command": "debugInfo", "success": false})
        );
        // Whatever it names the request by, the answer is the request's.
        let rich = json!({"type": "response", "sequence_seq": 9, "command": "evaluate",
                          "success": true, "body": {"result": "42"}});
        let answer = envelope(&rich, 4, "evaluate");
        assert_eq!(
            (
                &answer["request_seq"],
                &answer["success"],
                &answer["body"]["result"]
            ),
            (&json!(4), &json!(true), &json!("42"))
        );
        // Another request's answer, taken for this one's by ipykernel's client, fails.
        let threads = json!({"type": "response", "request_seq": 1, "command": "threads",
                             "success": true, "body": {"threads": []}});
        let taken = envelope(&threads, 5, "setBreakpoints");
        assert_eq!(
            (&taken["success"], taken.get("body")),
            (&json!(false), None)
        );
        let refused = json!({"command": "evaluate", "success": false, "message": "no frame"});
        assert_eq!(envelope(&refused, 6, "evaluate")["message"], "no frame");
    }
}
