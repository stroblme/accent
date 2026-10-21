//! One-shot, vault-scoped MCP access to the same holder that owns GTK terminals.

use super::client;
use super::protocol::{self, ACK, CREATE, ERROR, Hello, LIST_OWNED, OUTPUT, READ, SEND, STOP};
use std::fs::File;
use std::io::{self, Read as _};
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

fn cwd(vault: &Path, relative: &str) -> io::Result<PathBuf> {
    if !Path::new(relative)
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(io::Error::other(
            "terminal cwd must be relative to the vault",
        ));
    }
    let root = vault.canonicalize()?;
    let path = root.join(relative).canonicalize()?;
    if !path.starts_with(root) || !path.is_dir() {
        return Err(io::Error::other(
            "terminal cwd is not a directory inside the vault",
        ));
    }
    Ok(path)
}

fn random_id() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn request(conn: &mut UnixStream, tag: u8, payload: &[u8]) -> io::Result<(u8, Vec<u8>)> {
    conn.set_read_timeout(Some(Duration::from_secs(5)))?;
    protocol::write_frame(conn, tag, payload)?;
    match protocol::read_frame(conn)? {
        Some((ERROR, reason)) => Err(io::Error::other(
            String::from_utf8_lossy(&reason).into_owned(),
        )),
        Some(reply) => Ok(reply),
        None => Err(io::Error::other("terminal holder gave no answer")),
    }
}

fn existing(tag: u8, payload: &[u8]) -> io::Result<(u8, Vec<u8>)> {
    let mut conn = client::holder()?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no terminal holder"))?;
    request(&mut conn, tag, payload)
}

fn scoped(vault: &Path, id: &str) -> io::Result<Value> {
    Ok(json!({"id": id, "owner": super::mcp_owner(vault)?}))
}

fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn attach_command(vault: &Path, id: &str) -> io::Result<String> {
    let exe = std::env::current_exe()?;
    let exe = exe
        .to_str()
        .ok_or_else(|| io::Error::other("server executable path is not UTF-8"))?;
    let vault = vault.canonicalize()?;
    let vault = vault
        .to_str()
        .ok_or_else(|| io::Error::other("vault path is not UTF-8"))?;
    Ok(format!(
        "{} attach --existing --vault {} {}",
        quoted(exe),
        quoted(vault),
        quoted(id)
    ))
}

fn offer_attach(vault: &Path, value: &mut Value) -> io::Result<()> {
    if value["attached"] != true
        && let Some(id) = value["id"].as_str()
    {
        let command = attach_command(vault, id)?;
        value["attach_command"] = json!(command);
        value["run_on"] = json!("host");
    }
    Ok(())
}

/// Start an interactive shell without taking a GTK terminal attachment.
pub fn start(vault: &Path, relative: &str) -> io::Result<Value> {
    let cwd = cwd(vault, relative)?;
    let owner = super::mcp_owner(vault)?;
    let id = random_id()?;
    let hello = Hello {
        id: id.clone(),
        cwd: cwd.clone(),
        owner,
        rows: 24,
        cols: 80,
        env: std::env::vars_os().collect(),
    };
    let (tag, _) = request(&mut client::connect()?, CREATE, &hello.encode())?;
    if tag != ACK {
        return Err(io::Error::other("terminal holder did not create a shell"));
    }
    Ok(
        json!({"id": id, "cwd": cwd.to_string_lossy(), "attach_command": attach_command(vault, &id)?, "run_on": "host"}),
    )
}

/// List this vault's MCP shells and GTK terminals explicitly shared with it.
pub fn list(vault: &Path) -> io::Result<Vec<Value>> {
    let Some(mut conn) = client::holder()? else {
        return Ok(Vec::new());
    };
    let owner = super::mcp_owner(vault)?;
    let (tag, bytes) = request(&mut conn, LIST_OWNED, owner.as_bytes())?;
    if tag != OUTPUT {
        return Err(io::Error::other("terminal holder did not list shells"));
    }
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut value: Value = serde_json::from_slice(line).map_err(io::Error::other)?;
            offer_attach(vault, &mut value)?;
            Ok(value)
        })
        .collect()
}

/// Read recent PTY output by absolute byte cursor, with a current plain-text screen snapshot.
pub fn read(vault: &Path, id: &str, offset: u64, limit: Option<usize>) -> io::Result<Value> {
    let mut input = scoped(vault, id)?;
    input["offset"] = json!(offset);
    input["limit"] = json!(limit.unwrap_or(64 * 1024).min(64 * 1024));
    let (tag, bytes) = existing(READ, input.to_string().as_bytes())?;
    if tag != OUTPUT {
        return Err(io::Error::other("terminal holder did not read shell"));
    }
    let mut value: Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    value["id"] = json!(id);
    offer_attach(vault, &mut value)?;
    Ok(value)
}

/// Send literal terminal input. The caller includes Enter as `\n` when wanted.
pub fn send(vault: &Path, id: &str, input: &str) -> io::Result<()> {
    if input.len() > 64 * 1024 {
        return Err(io::Error::other("terminal input exceeds 64 KiB"));
    }
    let mut payload = scoped(vault, id)?;
    payload["input"] = json!(input);
    let (tag, _) = existing(SEND, payload.to_string().as_bytes())?;
    if tag != ACK {
        return Err(io::Error::other("terminal holder did not accept input"));
    }
    Ok(())
}

/// Hang up the owned terminal or one explicitly shared with this vault.
pub fn stop(vault: &Path, id: &str) -> io::Result<()> {
    let payload = scoped(vault, id)?;
    let (tag, _) = existing(STOP, payload.to_string().as_bytes())?;
    if tag != ACK {
        return Err(io::Error::other("terminal holder did not stop shell"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attached_terminal_has_no_takeover_command() {
        let mut attached = json!({"id": "gui", "attached": true});
        offer_attach(Path::new("/"), &mut attached).unwrap();
        assert!(attached.get("attach_command").is_none());

        let mut detached = json!({"id": "mcp", "attached": false});
        offer_attach(Path::new("/"), &mut detached).unwrap();
        assert!(
            detached["attach_command"]
                .as_str()
                .unwrap()
                .contains(" attach --existing --vault '/' 'mcp'")
        );
        assert_eq!(detached["run_on"], "host");
    }

    #[test]
    fn host_attach_command_quotes_executable_and_vault_paths() {
        assert_eq!(quoted("/tmp/a b'c"), "'/tmp/a b'\\''c'");
    }
}
