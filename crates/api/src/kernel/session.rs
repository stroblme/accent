//! One kernel's session: its start, its messages turned into each run's [`News`], and the
//! requests the window makes of it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, Weak};

use accent_jupyter::{Channel, Header, Incoming, Incomings, Kernel};
use serde_json::{Value, json};

use super::{Info, Kernels, News, Status};
use crate::{Error, Event, Result, locked};

/// A running kernel and the runs it has been sent.
pub(super) struct Session {
    kernel: Kernel,
    runs: Mutex<Runs>,
    /// Being stopped ([`Kernels`]' `stop`): its end is then no news of its own.
    pub(super) stopping: AtomicBool,
}

/// Start `argv` for `notebook`, in its folder, its sockets under `under`, and register it as
/// `id`: answered once it answers.
pub(super) async fn start(
    kernels: Arc<Kernels>,
    id: u64,
    notebook: PathBuf,
    argv: Vec<String>,
    under: PathBuf,
    host: bool,
    interpreter: String,
) -> Result<Info> {
    let cwd = notebook.parent().unwrap_or(&kernels.root);
    if !host && let Err(e) = Kernel::sweep(&under) {
        tracing::debug!(
            "cannot remove old kernel folders in {}: {e}",
            under.display()
        );
    }
    // As jupyter_client launches one: ipykernel then ends itself once we are gone, a crash or a
    // kill included, rather than outliving us.
    let parent = [("JPY_PARENT_PID".to_string(), std::process::id().to_string())];
    let started = Kernel::start(&argv, cwd, &parent, &under).await;
    let (kernel, incomings, info) = started.map_err(failed)?;
    let text = |key: &str| info[key].as_str().unwrap_or_default().to_string();
    let info = Info {
        interpreter,
        implementation: text("implementation"),
        version: text("implementation_version"),
    };
    let session = Arc::new(Session {
        kernel,
        runs: Mutex::default(),
        stopping: AtomicBool::new(false),
    });
    // A vault shut down meanwhile stopped only the kernels it held, so this one stops its own.
    if !kernels.register(id, &session) {
        session.kernel.shutdown().await;
        return Err(accent_core::Error::Cancelled.into());
    }
    accent_lsp::runtime().spawn(forward(
        id,
        incomings,
        Arc::downgrade(&session),
        Arc::downgrade(&kernels),
        kernels.events.clone(),
    ));
    Ok(info)
}

impl Session {
    /// Send `code` as run `run`: its news comes under that number.
    pub(super) fn execute(&self, run: u64, code: String) -> Result<()> {
        let request = json!({
            "code": code, "silent": false, "store_history": true, "user_expressions": {},
            "allow_stdin": true, "stop_on_error": true,
        });
        // Registered under the lock the news is read under, so no answer comes before it.
        let mut runs = locked(&self.runs);
        let sent = self
            .kernel
            .send(Channel::Shell, "execute_request", request, None);
        runs.register(sent.map_err(failed)?, run);
        Ok(())
    }

    /// Answer the `input()` that asks.
    pub(super) fn input(&self, value: String) -> Result<()> {
        let asking = locked(&self.runs).asking.take();
        let Some((_, asking)) = asking else {
            return Err(Error::Language("nothing asks for input".to_string()));
        };
        let reply = json!({ "value": value });
        let sent = self
            .kernel
            .send(Channel::Stdin, "input_reply", reply, Some(&asking));
        sent.map(drop).map_err(failed)
    }

    pub(super) fn interrupt(&self) -> Result<()> {
        self.kernel
            .interrupt()
            .map_err(|e| Error::Language(format!("cannot interrupt the kernel: {e}")))
    }

    pub(super) async fn stop(&self) {
        self.kernel.shutdown().await;
    }
}

fn failed(e: accent_jupyter::Error) -> Error {
    Error::Language(e.to_string())
}

/// Tell the window each run's news until the kernel ends; then say it is over, once. Holds the
/// session weakly, so a stopped one is dropped at once.
async fn forward(
    id: u64,
    mut incomings: Incomings,
    session: Weak<Session>,
    kernels: Weak<Kernels>,
    events: Sender<Event>,
) {
    let emit = |news| {
        let _ = events.send(Event::Kernel { session: id, news });
    };
    let mut code = None;
    while let Some(incoming) = incomings.recv().await {
        if let Incoming::Exited(exited) = incoming {
            code = exited;
            break;
        }
        let Some(session) = session.upgrade() else {
            break;
        };
        let news = locked(&session.runs).take(&incoming);
        news.into_iter().for_each(emit);
    }
    // Still registered and not being stopped, it ended on its own.
    let ended = kernels.upgrade().and_then(|k| k.remove(id));
    let error = ended
        .filter(|s| !s.stopping.load(Ordering::SeqCst))
        .map(|session| match (session.kernel.last_words(), code) {
            (Some(words), _) => words,
            (None, Some(code)) => format!("exit code {code}"),
            (None, None) => "it was killed".to_string(),
        });
    emit(News::Ended { error });
}

/// The runs a kernel has been sent, by their `execute_request`'s id, and what each still waits
/// for: a run is done once both its shell reply and IOPub's `idle` after it have come, in either
/// order, outputs arriving until the second.
#[derive(Default)]
pub(crate) struct Runs {
    pending: HashMap<String, Pending>,
    /// The `input_request` waiting for an answer, and its run.
    asking: Option<(u64, Header)>,
}

struct Pending {
    run: u64,
    reply: Option<(Status, Option<u32>)>,
    idle: bool,
}

impl Runs {
    /// Expect the messages caused by `msg_id`, which is run `run`.
    pub(crate) fn register(&mut self, msg_id: String, run: u64) {
        let pending = Pending {
            run,
            reply: None,
            idle: false,
        };
        self.pending.insert(msg_id, pending);
    }

    /// What `incoming` says of a run; nothing for a message no run caused.
    pub(crate) fn take(&mut self, incoming: &Incoming) -> Vec<News> {
        let Incoming::Message(channel, msg) = incoming else {
            return Vec::new();
        };
        let Some(parent) = msg.parent.as_ref().map(|p| p.msg_id.as_str()) else {
            return Vec::new();
        };
        let Some(pending) = self.pending.get_mut(parent) else {
            return Vec::new();
        };
        let (run, content) = (pending.run, &msg.content);
        let count = || {
            content["execution_count"]
                .as_u64()
                .and_then(|n| n.try_into().ok())
        };
        let news = match (channel, msg.header.msg_type.as_str()) {
            (Channel::Iopub, "execute_input") => Some(News::Started {
                run,
                count: count(),
            }),
            (Channel::Iopub, "clear_output") => Some(News::Clear {
                run,
                wait: content["wait"].as_bool().unwrap_or(false),
            }),
            (Channel::Iopub, "status") => {
                pending.idle |= content["execution_state"] == "idle";
                None
            }
            (Channel::Iopub, "update_display_data") => content["transient"]["display_id"]
                .as_str()
                .and_then(|display_id| {
                    nbformat("display_data", content).map(|output| News::Update {
                        display_id: display_id.to_string(),
                        output,
                    })
                }),
            (Channel::Iopub, msg_type) => nbformat(msg_type, content).map(|output| News::Output {
                run,
                output,
                display_id: content["transient"]["display_id"]
                    .as_str()
                    .map(String::from),
            }),
            (Channel::Shell, "execute_reply") => {
                let status = match content["status"].as_str() {
                    Some("ok") => Status::Ok,
                    Some("aborted") => Status::Aborted,
                    _ => Status::Error,
                };
                pending.reply = Some((status, count()));
                None
            }
            (Channel::Stdin, "input_request") => {
                self.asking = Some((run, msg.header.clone()));
                Some(News::Input {
                    run,
                    prompt: content["prompt"].as_str().unwrap_or_default().to_string(),
                    password: content["password"].as_bool().unwrap_or(false),
                })
            }
            _ => None,
        };
        let mut news: Vec<News> = news.into_iter().collect();
        if let Some(Pending {
            reply: Some((status, count)),
            idle: true,
            ..
        }) = self.pending.get(parent)
        {
            news.push(News::Done {
                run,
                status: *status,
                count: *count,
            });
            self.pending.remove(parent);
            if self.asking.as_ref().is_some_and(|(asked, _)| *asked == run) {
                self.asking = None;
            }
        }
        news
    }
}

/// An IOPub message as the output nbformat 4 stores (nbformat's `output_from_msg`); `None` for
/// one that is not an output. A display's `transient` id stays outside saved nbformat JSON.
fn nbformat(msg_type: &str, content: &Value) -> Option<Value> {
    let keys: &[&str] = match msg_type {
        "execute_result" => &["data", "metadata", "execution_count"],
        "display_data" => &["data", "metadata"],
        "stream" => &["name", "text"],
        "error" => &["ename", "evalue", "traceback"],
        _ => return None,
    };
    let mut output = serde_json::Map::new();
    output.insert("output_type".into(), msg_type.into());
    for key in keys {
        output.insert(key.to_string(), content[key].clone());
    }
    Some(output.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use accent_jupyter::Message;

    fn message(channel: Channel, msg_type: &str, content: Value) -> Incoming {
        let header = |msg_type: &str, msg_id: &str| Header {
            msg_type: msg_type.to_string(),
            msg_id: msg_id.to_string(),
            ..Header::default()
        };
        let msg = Message {
            header: header(msg_type, "theirs"),
            parent: Some(header("execute_request", "ours")),
            metadata: json!({}),
            content,
        };
        Incoming::Message(channel, Box::new(msg))
    }

    #[test]
    fn runs_turn_a_cells_messages_into_its_news() {
        let iopub = |msg_type, content| message(Channel::Iopub, msg_type, content);
        let stream = |text| json!({"output_type": "stream", "name": "stdout", "text": text});
        let result = json!({"data": {"text/plain": "1"}, "metadata": {}, "execution_count": 1});
        let ran = [
            iopub("status", json!({"execution_state": "busy"})),
            iopub("execute_input", json!({"code": "1", "execution_count": 1})),
            iopub("stream", json!({"name": "stdout", "text": "a"})),
            iopub("stream", json!({"name": "stdout", "text": "b"})),
            iopub("execute_result", result.clone()),
        ];
        let reply = message(
            Channel::Shell,
            "execute_reply",
            json!({"status": "ok", "execution_count": 1}),
        );
        let idle = iopub("status", json!({"execution_state": "idle"}));
        let mut result = result;
        result["output_type"] = "execute_result".into();
        for ending in [[reply.clone(), idle.clone()], [idle, reply]] {
            let mut runs = Runs::default();
            runs.register("ours".to_string(), 7);
            let news: Vec<News> = ran
                .iter()
                .chain(&ending)
                .flat_map(|i| runs.take(i))
                .collect();
            let output = |output| News::Output {
                run: 7,
                output,
                display_id: None,
            };
            let done = News::Done {
                run: 7,
                status: Status::Ok,
                count: Some(1),
            };
            let started = News::Started {
                run: 7,
                count: Some(1),
            };
            let expected = [
                started,
                output(stream("a")),
                output(stream("b")),
                output(result.clone()),
                done,
            ];
            assert_eq!(news, expected);
            assert!(runs.pending.is_empty());
        }
    }

    /// After an error, the kernel answers the runs queued behind it without running them.
    #[test]
    fn an_aborted_run_is_done_without_starting() {
        let mut runs = Runs::default();
        runs.register("ours".to_string(), 3);
        let aborted = [
            message(Channel::Iopub, "status", json!({"execution_state": "busy"})),
            message(
                Channel::Shell,
                "execute_reply",
                json!({"status": "aborted"}),
            ),
            message(Channel::Iopub, "status", json!({"execution_state": "idle"})),
        ];
        let news: Vec<News> = aborted.iter().flat_map(|i| runs.take(i)).collect();
        let done = News::Done {
            run: 3,
            status: Status::Aborted,
            count: None,
        };
        assert_eq!(news, [done]);
    }

    #[test]
    fn a_display_update_keeps_its_id_out_of_nbformat_output() {
        let mut runs = Runs::default();
        runs.register("ours".to_string(), 7);
        let display = |kind, text| {
            message(
                Channel::Iopub,
                kind,
                json!({
                    "data": {"text/plain": text},
                    "metadata": {},
                    "transient": {"display_id": "shared"}
                }),
            )
        };
        let output = json!({
            "output_type": "display_data",
            "data": {"text/plain": "before"},
            "metadata": {}
        });
        assert_eq!(
            runs.take(&display("display_data", "before")),
            [News::Output {
                run: 7,
                output,
                display_id: Some("shared".into()),
            }]
        );
        assert_eq!(
            runs.take(&display("update_display_data", "after")),
            [News::Update {
                display_id: "shared".into(),
                output: json!({
                    "output_type": "display_data",
                    "data": {"text/plain": "after"},
                    "metadata": {}
                }),
            }]
        );
    }
}
