//! One kernel's session: its start, its messages turned into each run's [`News`], and the
//! requests the window makes of it. What a run's news carries is bounded here, where the kernel
//! is, as a notebook keeps output ([`bound`]): every queue after it, to a local window or over a
//! remote vault's link, holds no more than a cell will.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use accent_core::notebook::bound::{self, Stream};
use accent_jupyter::{Channel, Header, Incoming, Incomings, Kernel};
use serde_json::{Value, json};

use super::{Info, Kernels, Launch, News, Status};
use crate::{Error, Event, Result, locked};

/// How long a run's stream text waits to go with what follows it: a flood's thousands of
/// messages a second go as ten.
const BATCH: Duration = Duration::from_millis(100);
/// The most stream text a run sends a second, its batches waiting longer past it: the tail of
/// a flood, cut and moved on, is all new at each batch.
const RATE: f64 = (1 << 20) as f64;

/// A running kernel and the runs it has been sent.
pub(super) struct Session {
    kernel: Kernel,
    runs: Mutex<Runs>,
    /// Being stopped ([`Kernels`]' `stop`): its end is then no news of its own.
    pub(super) stopping: AtomicBool,
    /// Interrupted with an `interrupt_request` on control rather than SIGINT.
    message_interrupt: bool,
}

/// Start `launch` for `notebook`, in its folder, and register it as `id`: answered once it
/// answers.
pub(super) async fn start(
    kernels: Arc<Kernels>,
    id: u64,
    notebook: PathBuf,
    launch: Launch,
) -> Result<Info> {
    let (argv, under, host) = super::command(launch.argv, &launch.env, &notebook);
    let cwd = notebook.parent().unwrap_or(&kernels.root);
    if !host && let Err(e) = Kernel::sweep(&under) {
        tracing::debug!(
            "cannot remove old kernel folders in {}: {e}",
            under.display()
        );
    }
    // As jupyter_client launches one: ipykernel then ends itself once we are gone, a crash or a
    // kill included, rather than outliving us.
    let parent = ("JPY_PARENT_PID".to_string(), std::process::id().to_string());
    let env: Vec<(String, String)> = launch.env.into_iter().chain([parent]).collect();
    let started = Kernel::start(&argv, cwd, &env, &under).await;
    // A kernel that binds `tcp` whatever the connection file says is never reached.
    let (kernel, incomings, info) = started.map_err(|e| match e {
        accent_jupyter::Error::Timeout(_) if launch.kernelspec => Error::Language(format!(
            "{e}: Accent reaches kernels over the ipc transport, which {} may not take",
            launch.called
        )),
        e => failed(e),
    })?;
    let text = |key: &str| info[key].as_str().unwrap_or_default().to_string();
    let info = Info {
        interpreter: launch.called,
        implementation: text("implementation"),
        version: text("implementation_version"),
    };
    let session = Arc::new(Session {
        kernel,
        runs: Mutex::default(),
        stopping: AtomicBool::new(false),
        message_interrupt: launch.message_interrupt,
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
        if self.message_interrupt {
            let sent = self
                .kernel
                .send(Channel::Control, "interrupt_request", json!({}), None);
            return sent.map(drop).map_err(failed);
        }
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
    // Stream text waits for its batch, and goes when that is due if nothing comes first.
    while let Some(due) = session.upgrade().map(|s| locked(&s.runs).due()) {
        let next = match due {
            Some(due) => tokio::time::timeout_at(due.into(), incomings.recv())
                .await
                .ok(),
            None => Some(incomings.recv().await),
        };
        let Some(session) = session.upgrade() else {
            break;
        };
        let news = match next {
            None => locked(&session.runs).flush(Instant::now()),
            Some(None) => break,
            Some(Some(Incoming::Exited(exited))) => {
                code = exited;
                break;
            }
            Some(Some(incoming)) => locked(&session.runs).take(&incoming, Instant::now()),
        };
        news.into_iter().for_each(emit);
    }
    if let Some(session) = session.upgrade() {
        let waiting = locked(&session.runs).flush(Instant::now() + BATCH);
        waiting.into_iter().for_each(emit);
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
    outputs: Outputs,
}

impl Runs {
    /// Expect the messages caused by `msg_id`, which is run `run`.
    pub(crate) fn register(&mut self, msg_id: String, run: u64) {
        let pending = Pending {
            run,
            reply: None,
            idle: false,
            outputs: Outputs::default(),
        };
        self.pending.insert(msg_id, pending);
    }

    /// When the first stream text waiting is due.
    pub(crate) fn due(&self) -> Option<Instant> {
        self.pending.values().filter_map(|p| p.outputs.due()).min()
    }

    /// The stream text due by `now`.
    pub(crate) fn flush(&mut self, now: Instant) -> Vec<News> {
        let due = self.pending.values_mut();
        let due = due.filter(|p| p.outputs.due().is_some_and(|due| due <= now));
        due.flat_map(|p| p.outputs.flush(p.run, now)).collect()
    }

    /// What `incoming` says of a run, a stream's text once its batch is due; nothing for a
    /// message no run caused.
    pub(crate) fn take(&mut self, incoming: &Incoming, now: Instant) -> Vec<News> {
        if let Incoming::Dropped {
            msg_type,
            parent,
            bytes,
            ..
        } = incoming
        {
            let output = [
                "stream",
                "display_data",
                "execute_result",
                "update_display_data",
            ]
            .contains(&msg_type.as_deref().unwrap_or_default());
            let pending = parent.as_ref().and_then(|p| self.pending.get_mut(p));
            let Some(pending) = pending.filter(|_| output) else {
                return Vec::new();
            };
            let why = bound::too_large(*bytes, accent_jupyter::MAX_MESSAGE);
            return pending.outputs.note(pending.run, &why, false, now);
        }
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
        if (*channel, msg.header.msg_type.as_str()) == (Channel::Iopub, "stream") {
            let name = content["name"].as_str().unwrap_or("stdout");
            let text = content["text"].as_str().unwrap_or_default();
            return pending.outputs.stream(run, name, text, false, now);
        }
        // Whatever else a run says comes after the stream text before it.
        let mut news = pending.outputs.flush(run, now);
        let count = || {
            content["execution_count"]
                .as_u64()
                .and_then(|n| n.try_into().ok())
        };
        let said = match (channel, msg.header.msg_type.as_str()) {
            (Channel::Iopub, "execute_input") => Some(News::Started {
                run,
                count: count(),
            }),
            (Channel::Iopub, "clear_output") => {
                pending.outputs = Outputs::default();
                Some(News::Clear {
                    run,
                    wait: content["wait"].as_bool().unwrap_or(false),
                })
            }
            (Channel::Iopub, "status") => {
                pending.idle |= content["execution_state"] == "idle";
                None
            }
            (Channel::Iopub, "update_display_data") => content["transient"]["display_id"]
                .as_str()
                .and_then(|display_id| {
                    nbformat("display_data", content).map(|output| News::Update {
                        display_id: display_id.to_string(),
                        output: bounded(output),
                    })
                }),
            (Channel::Iopub, msg_type) => nbformat(msg_type, content)
                .and_then(|output| pending.outputs.output(msg_type, output))
                .map(|output| News::Output {
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
        news.extend(said);
        if let Some(Pending {
            reply: Some((status, count)),
            idle: true,
            outputs,
            ..
        }) = self.pending.get_mut(parent)
        {
            news.extend(outputs.end(run, now));
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

/// A run's outputs as they went to the window: how many count against [`bound::OUTPUTS`] and
/// how many more were left out, and its last output while that is a stream.
#[derive(Default)]
struct Outputs {
    kept: usize,
    left_out: usize,
    stream: Option<Open>,
}

/// The stream that is a run's last output.
struct Open {
    name: String,
    /// What the kernel sent of it, as a notebook keeps it; `None` for one left out.
    text: Option<Stream>,
    /// What the window has of it; `None` before its first batch.
    sent: Option<String>,
    /// When the text not yet sent is due.
    due: Option<Instant>,
    /// When the next batch may go, as [`RATE`] allows.
    next: Instant,
}

impl Outputs {
    fn due(&self) -> Option<Instant> {
        self.stream.as_ref()?.due
    }

    /// More of stream `name`: the last output's when that is a stream of that name, as Jupyter
    /// joins them, else a new output, left out past [`bound::OUTPUTS`] unless `always`.
    fn stream(
        &mut self,
        run: u64,
        name: &str,
        text: &str,
        always: bool,
        now: Instant,
    ) -> Vec<News> {
        let mut news = Vec::new();
        if self.stream.as_ref().is_none_or(|open| open.name != name) {
            news.extend(self.flush(run, now));
            let kept = always || self.count();
            self.stream = Some(Open {
                name: name.to_string(),
                text: kept.then(Stream::default),
                sent: None,
                due: None,
                next: now,
            });
        }
        if let Some(open) = &mut self.stream
            && let Some(stream) = &mut open.text
        {
            stream.push(text);
            open.due.get_or_insert(open.next.max(now + BATCH));
        }
        news
    }

    /// A line saying `why` an output is not kept, a stderr stream's.
    fn note(&mut self, run: u64, why: &str, always: bool, now: Instant) -> Vec<News> {
        let unended = self.stream.as_ref().filter(|open| open.name == "stderr");
        let unended = unended.and_then(|open| open.text.as_ref());
        let line = match unended.is_some_and(|text| !text.text().ends_with('\n')) {
            true => format!("\n{}", bound::line(why)),
            false => bound::line(why),
        };
        self.stream(run, "stderr", &line, always, now)
    }

    /// Whether one more output is kept, counting it either way.
    fn count(&mut self) -> bool {
        let kept = self.kept < bound::OUTPUTS;
        match kept {
            true => self.kept += 1,
            false => self.left_out += 1,
        }
        kept
    }

    /// Display, result or error `output`, of `msg_type`, as the window gets it, after the stream
    /// text before it: none for a display past [`bound::OUTPUTS`], whereas a result and an error
    /// are always kept.
    fn output(&mut self, msg_type: &str, output: Value) -> Option<Value> {
        self.stream = None;
        (msg_type != "display_data" || self.count()).then(|| bounded(output))
    }

    /// The stream text not yet sent, as the changes to what the window has of it.
    fn flush(&mut self, run: u64, now: Instant) -> Vec<News> {
        let Some(open) = self.stream.as_mut().filter(|open| open.due.is_some()) else {
            return Vec::new();
        };
        open.due = None;
        let Some(text) = open.text.as_ref().map(Stream::text) else {
            return Vec::new();
        };
        let changes = match &open.sent {
            None => vec![(None, 0, text.as_str())],
            Some(sent) => bound::diff(sent, &text)
                .into_iter()
                .filter(|(range, piece)| !range.is_empty() || !piece.is_empty())
                .map(|(range, piece)| (Some(range.start), range.len(), piece))
                .collect(),
        };
        let bytes: usize = changes.iter().map(|(.., piece)| piece.len()).sum();
        open.next = now + BATCH.max(Duration::from_secs_f64(bytes as f64 / RATE));
        let news = changes
            .into_iter()
            .map(|(at, replaced, piece)| News::Stream {
                run,
                name: open.name.clone(),
                at,
                replaced,
                text: piece.to_string(),
            })
            .collect();
        open.sent = Some(text);
        news
    }

    /// The run is done: a note counting the outputs left out, and what stream text waits.
    fn end(&mut self, run: u64, now: Instant) -> Vec<News> {
        let mut news = Vec::new();
        if self.left_out > 0 {
            let why = bound::too_many(self.left_out);
            news.extend(self.note(run, &why, true, now));
            self.left_out = 0;
        }
        news.extend(self.flush(run, now));
        news
    }
}

/// A display or result as the window gets it: a note in the place of one past [`bound::DATA`].
fn bounded(output: Value) -> Value {
    let bytes = bound::data_size(&output["data"]);
    if bytes <= bound::DATA {
        return output;
    }
    let data = output["data"].as_object();
    let mimes = data.into_iter().flat_map(|d| d.keys());
    let mime = mimes
        .clone()
        .find(|m| *m != "text/plain")
        .or(mimes.clone().next());
    bound::note(&bound::too_much_data(
        bytes,
        mime.map_or("", String::as_str),
    ))
}

/// An IOPub message as the output nbformat 4 stores (nbformat's `output_from_msg`); `None` for
/// one that is not an output, or a stream, which [`Outputs::stream`] keeps. A display's
/// `transient` id stays outside saved nbformat JSON.
fn nbformat(msg_type: &str, content: &Value) -> Option<Value> {
    let keys: &[&str] = match msg_type {
        "execute_result" => &["data", "metadata", "execution_count"],
        "display_data" => &["data", "metadata"],
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
                .flat_map(|i| runs.take(i, Instant::now()))
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
                // Joined while they wait for their batch, which what follows them sends.
                News::Stream {
                    run: 7,
                    name: "stdout".into(),
                    at: None,
                    replaced: 0,
                    text: "ab".into(),
                },
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
        let news: Vec<News> = aborted
            .iter()
            .flat_map(|i| runs.take(i, Instant::now()))
            .collect();
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
            runs.take(&display("display_data", "before"), Instant::now()),
            [News::Output {
                run: 7,
                output,
                display_id: Some("shared".into()),
            }]
        );
        assert_eq!(
            runs.take(&display("update_display_data", "after"), Instant::now()),
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

    /// A flood a kernel could send in a few seconds: what reaches the window is what a notebook
    /// keeps, in batches of at most a mebibyte a second, and the run still ends.
    #[test]
    fn a_flood_reaches_the_window_bounded_and_batched() {
        let iopub = |msg_type, content| message(Channel::Iopub, msg_type, content);
        let line = format!("{}\n", "x".repeat(999));
        let mut runs = Runs::default();
        runs.register("ours".to_string(), 1);
        let start = Instant::now();
        let mut news = Vec::new();
        let mut take = |incoming: Incoming, ms: u64| {
            let now = start + Duration::from_millis(ms);
            news.extend(runs.take(&incoming, now));
            if runs.due().is_some_and(|due| due <= now) {
                news.extend(runs.flush(now));
            }
        };
        // 64 MB of stream, a message a millisecond.
        for ms in 0..1000 {
            take(
                iopub("stream", json!({"name": "stdout", "text": line.repeat(64)})),
                ms,
            );
        }
        let display = json!({"data": {"text/plain": "1"}, "metadata": {}});
        for ms in 1000..2000 {
            take(iopub("display_data", display.clone()), ms);
        }
        let picture = json!({"data": {"image/png": "A".repeat(bound::DATA + 1)}, "metadata": {}});
        take(iopub("execute_result", picture), 2000);
        let reply = json!({"status": "ok", "execution_count": 1});
        take(message(Channel::Shell, "execute_reply", reply), 2001);
        take(iopub("status", json!({"execution_state": "idle"})), 2002);

        // The window's text, as it applies each change.
        let (mut text, mut sent) = (String::new(), 0);
        for n in &news {
            if let News::Stream {
                at,
                replaced,
                text: more,
                name,
                ..
            } = n
                && name == "stdout"
            {
                match at {
                    Some(at) => text.replace_range(*at..at + replaced, more),
                    None => text.clone_from(more),
                }
                sent += more.len();
            }
        }
        // A batch of all the bound keeps, then one more a second at most.
        assert!(sent <= 2 * (bound::HEAD + bound::TAIL), "{sent} bytes sent");
        assert!(
            text.len() <= bound::HEAD + bound::TAIL + 100,
            "{}",
            text.len()
        );
        assert!(text.contains(" left out …\n"), "the stream says it was cut");
        let outputs = |n: &&News| matches!(n, News::Output { .. });
        assert_eq!(
            news.iter().filter(outputs).count(),
            499 + 1,
            "the displays kept, the result"
        );
        let Some(News::Output { output: result, .. }) = news.iter().rfind(outputs) else {
            unreachable!()
        };
        assert!(
            result["text"]
                .as_str()
                .unwrap()
                .contains("(image/png) left out")
        );
        let [
            ..,
            News::Stream {
                text: note,
                at: None,
                ..
            },
            News::Done { .. },
        ] = &news[..]
        else {
            panic!("{:?}", &news[news.len() - 2..]);
        };
        assert_eq!(
            note,
            "… 501 more outputs left out, past the 500 a cell keeps …\n"
        );
    }
}
