//! Draw a diagram's page through the accent app beside this binary (`accent --render-diagram`):
//! `accent-cli` has no toolkit to lay out and draw text with, and the app's own export is what
//! the reader sees. Where there is no app or no display, as on a remote vault's host, the tool
//! says so instead.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

/// How long a page may take before the app drawing it is stopped: it takes well under a second.
const DEADLINE: Duration = Duration::from_secs(15);

/// Page `page` (from 0) of the diagram `xml` as a PNG.
pub(super) fn png(xml: &str, page: usize) -> Result<Vec<u8>, String> {
    let app = std::env::current_exe()
        .map(|exe| exe.with_file_name("accent"))
        .ok()
        .filter(|app| app.is_file())
        .ok_or(
            "drawing a page needs the accent app beside accent-cli, and this machine has none",
        )?;
    let mut child = Command::new(app)
        .arg("--render-diagram")
        .arg(page.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("the accent app did not start: {e}"))?;
    // Fed and read on threads of their own, so that neither end waits on a full pipe.
    if let Some(mut input) = child.stdin.take() {
        let xml = xml.as_bytes().to_vec();
        std::thread::spawn(move || input.write_all(&xml));
    }
    let (out, err) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let started = Instant::now();
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status,
            None if started.elapsed() > DEADLINE => {
                let _ = child.kill();
                let _ = child.wait();
                let secs = DEADLINE.as_secs();
                return Err(format!(
                    "the accent app took more than {secs} s to draw the page"
                ));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    // Bounded too: anything the app started could still hold the pipes open.
    let read = |pipe: Receiver<Vec<u8>>| {
        pipe.recv_timeout(Duration::from_secs(2))
            .unwrap_or_default()
    };
    match status.success() {
        true => Ok(read(out)),
        false => Err(format!(
            "the accent app could not draw the page: {}",
            reason(&String::from_utf8_lossy(&read(err)))
        )),
    }
}

/// Everything `pipe` says, once it closes.
fn drain(pipe: Option<impl Read + Send + 'static>) -> Receiver<Vec<u8>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        let _ = tx.send(bytes);
    });
    rx
}

/// The app's own word on why, out of what its libraries also printed: its `accent:` lines.
fn reason(stderr: &str) -> String {
    let own: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("accent: "))
        .collect();
    match own.is_empty() {
        true => stderr
            .lines()
            .rfind(|l| !l.trim().is_empty())
            .unwrap_or("it stopped")
            .to_string(),
        false => own.join("; "),
    }
}
