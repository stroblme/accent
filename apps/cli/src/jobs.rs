//! Detached commands for MCP. One short-lived supervisor owns each job and its stop socket.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read as _, Seek as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

const RETAIN: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const LOG_CAP: u64 = 16 * 1024 * 1024;
const SEGMENT_SIZE: u64 = LOG_CAP / 2;
const READ_CAP: usize = 256 * 1024;
const STOP_GRACE: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Queued,
    Running,
    Exited,
    Stopped,
    Interrupted,
    Failed,
}

impl State {
    fn terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Meta {
    id: String,
    cwd: String,
    state: State,
    started: u64,
    finished: Option<u64>,
    exit_code: Option<i32>,
    error: Option<String>,
    socket: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
pub struct Job {
    id: String,
    cwd: String,
    state: State,
    started: u64,
    finished: Option<u64>,
    exit_code: Option<i32>,
    error: Option<String>,
    truncated: bool,
}

impl Job {
    fn of(meta: Meta, dir: &Path) -> Self {
        Self {
            id: meta.id,
            cwd: meta.cwd,
            state: meta.state,
            started: meta.started,
            finished: meta.finished,
            exit_code: meta.exit_code,
            error: meta.error,
            truncated: dir.join("truncated").exists(),
        }
    }
}

#[derive(Serialize)]
pub struct Output {
    #[serde(flatten)]
    job: Job,
    output: String,
    base_offset: u64,
    gap: u64,
    next_offset: u64,
    eof: bool,
}

#[derive(Deserialize, Serialize)]
struct Spec {
    cwd: PathBuf,
    argv: Vec<String>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn private_dir(path: &Path) -> Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    let meta = fs::symlink_metadata(path)?;
    // SAFETY: getuid takes no arguments and cannot fail.
    let uid = unsafe { libc::getuid() };
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        bail!("{} is not a private directory of this user", path.display());
    }
    Ok(())
}

fn base(vault: &Path) -> Result<PathBuf> {
    let root = vault.canonicalize()?;
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .context("HOME is unset")?;
    let name = accent_api::ssh::hash_of(root.to_string_lossy().as_bytes());
    let path = state.join("accent/jobs").join(name);
    private_dir(&path)?;
    Ok(path)
}

fn valid_id(id: &str) -> Result<()> {
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid job id");
    }
    Ok(())
}

fn dir(root: &Path, id: &str) -> Result<PathBuf> {
    valid_id(id)?;
    Ok(root.join(id))
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let temp = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temp)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_data()?;
    fs::rename(temp, path)?;
    Ok(())
}

fn metadata(dir: &Path) -> Result<Meta> {
    Ok(serde_json::from_reader(File::open(dir.join("meta.json"))?)?)
}

fn save(dir: &Path, meta: &Meta) -> Result<()> {
    write_json(&dir.join("meta.json"), meta)
}

fn log_path(dir: &Path, segment: u64) -> PathBuf {
    dir.join(format!("output-{segment}.log"))
}

fn resolve_cwd(vault: &Path, cwd: &str) -> Result<PathBuf> {
    if !Path::new(cwd)
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        bail!("job cwd must be relative to the vault");
    }
    let root = vault.canonicalize()?;
    let path = root.join(cwd).canonicalize()?;
    if !path.starts_with(&root) || !path.is_dir() {
        bail!("job cwd is not a directory inside the vault");
    }
    Ok(path)
}

fn random_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Start a host-local job in a detached supervisor. Disconnecting MCP does not end it.
pub fn start(vault: &Path, cwd: &str, argv: &[String]) -> Result<Job> {
    if argv.is_empty() || argv[0].is_empty() {
        bail!("job argv must name a program");
    }
    let real_cwd = resolve_cwd(vault, cwd)?;
    let root = base(vault)?;
    prune_in(&root)?;
    let (id, dir) = loop {
        let id = random_id()?;
        let dir = root.join(&id);
        match DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => break (id, dir),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    };
    let prepared = (|| -> Result<_> {
        let socket = crate::hold::dir()?.join(format!("job-{id}.sock"));
        let meta = Meta {
            id: id.clone(),
            cwd: cwd.to_string(),
            state: State::Queued,
            started: now(),
            finished: None,
            exit_code: None,
            error: None,
            socket,
        };
        write_json(
            &dir.join("spec.json"),
            &Spec {
                cwd: real_cwd,
                argv: argv.to_vec(),
            },
        )?;
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(log_path(&dir, 0))?;
        save(&dir, &meta)?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["job-run", "--vault"])
            .arg(vault)
            .arg(&id)
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: setsid is async-signal-safe and called before the child execs.
        unsafe {
            command.pre_exec(|| match libc::setsid() {
                -1 => Err(io::Error::last_os_error()),
                _ => Ok(()),
            });
        }
        Ok((meta, command.spawn()?))
    })();
    let (mut meta, mut child) = match prepared {
        Ok(value) => value,
        Err(error) => {
            let _ = fs::remove_dir_all(&dir);
            return Err(error);
        }
    };
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    let until = Instant::now() + Duration::from_secs(2);
    while meta.state == State::Queued && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
        meta = metadata(&dir)?;
    }
    Ok(Job::of(meta, &dir))
}

fn current(dir: &Path) -> Result<Meta> {
    let mut meta = metadata(dir)?;
    if matches!(meta.state, State::Running | State::Queued)
        && (meta.state == State::Running || now().saturating_sub(meta.started) > 10)
        && UnixStream::connect(&meta.socket).is_err()
    {
        meta.state = State::Interrupted;
        meta.finished = Some(now());
        meta.error = Some("the job supervisor is gone".into());
        save(dir, &meta)?;
    }
    Ok(meta)
}

pub fn list(vault: &Path) -> Result<Vec<Job>> {
    let root = base(vault)?;
    prune_in(&root)?;
    let mut jobs = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let id = entry.file_name().to_string_lossy().into_owned();
        if valid_id(&id).is_ok()
            && entry.file_type()?.is_dir()
            && entry.path().join("meta.json").is_file()
        {
            let dir = entry.path();
            jobs.push(Job::of(current(&dir)?, &dir));
        }
    }
    jobs.sort_by(|a, b| b.started.cmp(&a.started).then_with(|| b.id.cmp(&a.id)));
    jobs.truncate(100);
    Ok(jobs)
}

pub fn read(vault: &Path, id: &str, offset: u64, limit: Option<usize>) -> Result<Output> {
    let dir = dir(&base(vault)?, id)?;
    let meta = current(&dir)?;
    let mut segments = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(number) = name
            .strip_prefix("output-")
            .and_then(|name| name.strip_suffix(".log"))
            .and_then(|number| number.parse::<u64>().ok())
            && entry.file_type()?.is_file()
        {
            segments.push((number, entry.path(), entry.metadata()?.len()));
        }
    }
    segments.sort_by_key(|(number, _, _)| *number);
    let (first, _, _) = segments.first().context("job output is missing")?;
    let (last, _, last_size) = segments.last().context("job output is missing")?;
    let base_offset = first * SEGMENT_SIZE;
    let end = last * SEGMENT_SIZE + last_size;
    let start = offset.max(base_offset).min(end);
    let gap = base_offset.saturating_sub(offset);
    let limit = limit.unwrap_or(64 * 1024).min(READ_CAP);
    let mut bytes = Vec::new();
    if limit > 0 {
        for (number, path, size) in &segments {
            let segment_start = number * SEGMENT_SIZE;
            let at = start + bytes.len() as u64;
            if at >= segment_start + size || at < segment_start {
                continue;
            }
            let mut file = File::open(path)?;
            file.seek(io::SeekFrom::Start(at - segment_start))?;
            let remaining = limit + 3 - bytes.len();
            (&mut file).take(remaining as u64).read_to_end(&mut bytes)?;
            if bytes.len() >= limit + 3 {
                break;
            }
        }
    }
    let count = complete_text_end(&bytes, limit, meta.state.terminal());
    let next_offset = start + count as u64;
    Ok(Output {
        job: Job::of(meta.clone(), &dir),
        output: String::from_utf8_lossy(&bytes[..count]).into_owned(),
        base_offset,
        gap,
        next_offset,
        eof: meta.state.terminal() && next_offset >= end,
    })
}

fn complete_text_end(bytes: &[u8], limit: usize, ended: bool) -> usize {
    let mut end = limit.min(bytes.len());
    while end < bytes.len() && bytes[end] & 0b1100_0000 == 0b1000_0000 {
        end += 1;
    }
    let mut at = 0;
    while at < end {
        match std::str::from_utf8(&bytes[at..end]) {
            Ok(_) => return end,
            Err(error) => {
                at += error.valid_up_to();
                match error.error_len() {
                    Some(invalid) => at += invalid,
                    None => return if ended { end } else { at },
                }
            }
        }
    }
    end
}

pub fn stop(vault: &Path, id: &str) -> Result<Job> {
    let dir = dir(&base(vault)?, id)?;
    let meta = current(&dir)?;
    if meta.state.terminal() {
        return Ok(Job::of(meta, &dir));
    }
    let mut conn = UnixStream::connect(&meta.socket).context("job supervisor is unavailable")?;
    conn.set_read_timeout(Some(Duration::from_secs(2)))?;
    conn.write_all(b"S")?;
    let mut ack = [0];
    conn.read_exact(&mut ack)?;
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        let meta = current(&dir)?;
        if meta.state.terminal() || Instant::now() >= until {
            return Ok(Job::of(meta, &dir));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn prune_in(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let id = entry.file_name().to_string_lossy().into_owned();
        if valid_id(&id).is_err() || !entry.file_type()?.is_dir() {
            continue;
        }
        let dir = entry.path();
        if !dir.join("meta.json").exists() {
            if entry.metadata()?.modified()?.elapsed().unwrap_or_default()
                >= Duration::from_secs(60)
            {
                fs::remove_dir_all(dir)?;
            }
            continue;
        }
        let meta = current(&dir)?;
        if meta.state.terminal()
            && meta
                .finished
                .is_some_and(|at| now().saturating_sub(at) >= RETAIN.as_secs())
        {
            fs::remove_dir_all(dir)?;
        }
    }
    Ok(())
}

pub fn prune(vault: &Path) -> Result<()> {
    prune_in(&base(vault)?)
}

struct Socket(PathBuf);

impl Drop for Socket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct GroupChild {
    child: Child,
    reaped: bool,
}

impl Drop for GroupChild {
    fn drop(&mut self) {
        if !self.reaped {
            // An unreaped leader reserves the process-group ID while it is signalled.
            unsafe { libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL) };
            let _ = self.child.wait();
        }
    }
}

fn record_output(dir: &Path, mut reader: impl io::Read, done: &AtomicBool) -> Result<()> {
    let mut segment = 0u64;
    let mut size = 0u64;
    let mut log = OpenOptions::new()
        .append(true)
        .open(log_path(dir, segment))?;
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let mut at = 0;
                while at < n {
                    if size == SEGMENT_SIZE {
                        log.sync_data()?;
                        segment += 1;
                        if segment >= 2 {
                            fs::remove_file(log_path(dir, segment - 2))?;
                            if segment == 2 {
                                File::create(dir.join("truncated"))?;
                            }
                        }
                        log = OpenOptions::new()
                            .create_new(true)
                            .write(true)
                            .mode(0o600)
                            .open(log_path(dir, segment))?;
                        size = 0;
                    }
                    let count = (n - at).min((SEGMENT_SIZE - size) as usize);
                    log.write_all(&buf[at..at + count])?;
                    at += count;
                    size += count as u64;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                if done.load(Ordering::Acquire) {
                    break;
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    log.sync_data()?;
    Ok(())
}

/// Internal detached process entry point. It owns the child and its live stop socket.
pub fn supervise(vault: &Path, id: &str) -> Result<()> {
    let dir = dir(&base(vault)?, id)?;
    let result = supervise_inner(&dir);
    if let Err(error) = &result
        && let Ok(mut meta) = metadata(&dir)
    {
        meta.state = State::Failed;
        meta.finished = Some(now());
        meta.error = Some(error.to_string());
        let _ = save(&dir, &meta);
    }
    result
}

fn supervise_inner(dir: &Path) -> Result<()> {
    let mut meta = metadata(dir)?;
    let spec: Spec = serde_json::from_reader(File::open(dir.join("spec.json"))?)?;
    let listener = UnixListener::bind(&meta.socket)?;
    let _socket = Socket(meta.socket.clone());
    fs::set_permissions(&meta.socket, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let (reader, writer) = UnixStream::pair()?;
    reader.set_read_timeout(Some(Duration::from_millis(100)))?;
    let out: OwnedFd = writer.try_clone()?.into();
    let err: OwnedFd = writer.into();
    let child = Command::new(&spec.argv[0])
        .args(&spec.argv[1..])
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .process_group(0)
        .spawn()?;
    let mut child = GroupChild {
        child,
        reaped: false,
    };
    let _ = fs::remove_file(dir.join("spec.json"));
    meta.state = State::Running;
    save(dir, &meta)?;
    let done = Arc::new(AtomicBool::new(false));
    let logger = {
        let done = done.clone();
        let dir = dir.to_path_buf();
        std::thread::spawn(move || record_output(&dir, reader, &done))
    };
    let mut stopped = false;
    let mut stop_at: Option<Instant> = None;
    let status = loop {
        if let Some(at) = stop_at {
            if at.elapsed() >= STOP_GRACE {
                // Keep the leader unreaped until this signal: its PID still reserves the
                // process-group ID even if it exited promptly on TERM.
                unsafe { libc::kill(-(child.child.id() as libc::pid_t), libc::SIGKILL) };
                let status = child.child.wait()?;
                child.reaped = true;
                break status;
            }
        } else if let Some(status) = child.child.try_wait()? {
            child.reaped = true;
            break status;
        }
        match listener.accept() {
            Ok((mut conn, _)) => {
                conn.set_read_timeout(Some(Duration::from_millis(100)))?;
                let mut byte = [0];
                if conn.read_exact(&mut byte).is_ok() && byte == *b"S" {
                    if !stopped {
                        // The live, unreaped group leader still owns this PID, preventing reuse.
                        unsafe { libc::kill(-(child.child.id() as libc::pid_t), libc::SIGTERM) };
                        stopped = true;
                        stop_at = Some(Instant::now());
                    }
                    let _ = conn.write_all(b"K");
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    done.store(true, Ordering::Release);
    logger
        .join()
        .map_err(|_| anyhow::anyhow!("job log writer panicked"))??;
    meta.state = if stopped {
        State::Stopped
    } else {
        State::Exited
    };
    meta.exit_code = status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal));
    meta.finished = Some(now());
    save(dir, &meta)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_running_state_write_does_not_leave_a_command_running() {
        let dir =
            std::env::temp_dir().join(format!("accent-job-write-fail-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let marker = dir.join("orphaned");
        let socket = dir.join("stop.sock");
        let meta = Meta {
            id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            cwd: String::new(),
            state: State::Queued,
            started: now(),
            finished: None,
            exit_code: None,
            error: None,
            socket,
        };
        save(&dir, &meta).unwrap();
        write_json(
            &dir.join("spec.json"),
            &Spec {
                cwd: dir.clone(),
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    format!("sleep 0.2; touch '{}'", marker.display()),
                ],
            },
        )
        .unwrap();
        fs::create_dir(dir.join("meta.tmp")).unwrap();
        assert!(supervise_inner(&dir).is_err());
        std::thread::sleep(Duration::from_millis(500));
        assert!(!marker.exists(), "the command survived its supervisor");
        fs::remove_dir_all(dir).unwrap();
    }
}
