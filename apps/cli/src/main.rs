//! Headless vault queries, MCP serving, remote RPC, and held shells. Queries update the
//! cache index using the app's scan policy.

mod hold;
mod mcp;

use accent_api::{Event, Progress, ReconcileStats, Vault, VaultConfig};
use accent_core::index::{Phase, default_db_path};
use accent_core::walk::{self, ScanOptions};
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Parser)]
#[command(
    name = "accent-cli",
    version,
    about = "accent vault index and query CLI, and the holder of its terminals' shells"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Args, Clone)]
struct Common {
    /// Vault root directory.
    #[arg(long)]
    vault: PathBuf,
    /// Index database. Defaults to `$XDG_CACHE_HOME/accent/<hash-of-vault-path>.db`
    #[arg(long)]
    db: Option<PathBuf>,
}

impl Common {
    fn db_path(&self) -> PathBuf {
        self.db
            .clone()
            .unwrap_or_else(|| default_db_path(&self.vault))
    }

    /// Open and reconcile the vault index using the app's scan policy, reporting progress.
    /// Stop watching when the command ends.
    fn open(&self, progress: impl Fn(Progress)) -> Result<(Vault, ReconcileStats)> {
        let (vault, events) =
            Vault::open_unwatched_at(&self.vault, &self.db_path(), VaultConfig::default())?;
        for event in events {
            match event {
                Event::Progress(p) => progress(p),
                Event::Reconciled(stats) => return Ok((vault, stats)),
                Event::Error(e) => anyhow::bail!(e),
                _ => {}
            }
        }
        anyhow::bail!("the vault stopped before its index was up to date")
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Reconcile the index with the vault.
    Index {
        #[command(flatten)]
        common: Common,
        /// Print progress after each batch.
        #[arg(long)]
        progress: bool,
    },
    /// Scan the vault without updating the database.
    Scan {
        #[command(flatten)]
        common: Common,
        /// List paths rejected by symlink rules.
        #[arg(long)]
        show_skipped: bool,
    },
    /// Search indexed file text.
    Search {
        #[command(flatten)]
        common: Common,
        query: Vec<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Include files ignored by Git.
        #[arg(long)]
        all: bool,
    },
    /// List notes linking to a file.
    Backlinks {
        #[command(flatten)]
        common: Common,
        rel_path: String,
    },
    /// List tags and use counts.
    Tags {
        #[command(flatten)]
        common: Common,
    },
    /// Summarise index contents.
    Stats {
        #[command(flatten)]
        common: Common,
    },
    /// Serve a vault over stdio JSON-RPC for the desktop's SSH remote backend. Exit when stdin
    /// closes, or after 90 s without messages from a client that has pinged, so dropped links do
    /// not leave servers behind.
    Serve {
        /// Vault root directory.
        #[arg(long)]
        vault: PathBuf,
        /// Index database. Defaults to `$XDG_CACHE_HOME/accent/<hash-of-vault-path>.db`
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Serve one index-free Git request for a held SSH shell over stdio.
    GitRpc,
    /// Serve a local vault over stdio MCP. Expose indexed reads and etag-checked writes
    /// until stdin closes. Run on the host for a remote vault.
    Mcp {
        #[command(flatten)]
        common: Common,
        /// Expose read-only MCP tools.
        #[arg(long)]
        read_only: bool,
    },
    /// Run one shell holder per user and machine. It exits after the last shell, or after ten
    /// idle seconds if no shell was started.
    Hold,
    /// Attach this terminal to a held shell, starting holder and shell if needed. Closing the
    /// terminal detaches. Exit with the shell status, 0 on detach, or 254 on failure
    /// (reserving 255 for SSH).
    Attach {
        /// Starting directory for a new shell; defaults to the current directory.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Owner shown by `held`; Accent passes its state directory for startup cleanup.
        #[arg(long, default_value = "")]
        owner: String,
        id: String,
    },
    /// End a held shell and its jobs. Pending attaches with this ID are cancelled.
    Kill { id: String },
    /// List held shells as JSON lines.
    Held,
    /// Print and clear the shell's last OSC 52 copy as base64.
    Clip { id: String },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "accent_cli=info,accent_core=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    match Cli::parse().cmd {
        Cmd::Index { common, progress } => {
            let t = Instant::now();
            let (_vault, stats) = common.open(|p| {
                if progress && p.phase == Phase::Index {
                    eprintln!("  indexing {}/{}", p.done, p.total);
                }
            })?;
            let total_ms = t.elapsed().as_millis();
            let db = common.db_path();
            println!("db            {}", db.display());
            println!("scanned       {}", stats.scanned);
            println!("unchanged     {}", stats.unchanged);
            println!("added         {}", stats.added);
            println!("updated       {}", stats.updated);
            println!(
                "touched       {}  (stat changed, content hash equal)",
                stats.touched
            );
            println!("removed       {}", stats.removed);
            println!("aliases       {}", stats.aliases);
            println!("conflicts     {}", stats.conflicts);
            println!("skipped links {}", stats.skipped_symlinks);
            println!("bytes read    {}", stats.bytes_read);
            println!("scan          {} ms", stats.scan_ms);
            println!(
                "reconcile     {} ms  (of which scan {} ms)",
                total_ms, stats.scan_ms
            );
            if let Ok(m) = std::fs::metadata(&db) {
                println!("db size       {} KiB", m.len() / 1024);
            }
        }
        Cmd::Scan {
            common,
            show_skipped,
        } => {
            let t = Instant::now();
            // The walk the vault's worker makes, without the index it would write.
            let r = walk::scan(&common.vault, &ScanOptions::default());
            let ms = t.elapsed().as_millis();
            let dirs = r
                .files
                .iter()
                .filter(|f| f.kind == walk::FileKind::Dir)
                .count();
            let md = r
                .files
                .iter()
                .filter(|f| f.kind == walk::FileKind::Markdown)
                .count();
            let conflicts = r
                .files
                .iter()
                .filter(|f| f.kind == walk::FileKind::Conflict)
                .count();
            println!("entries    {}  ({dirs} dirs, {md} markdown)", r.files.len());
            println!("aliases    {}", r.aliases.len());
            println!("conflicts  {conflicts}");
            println!("skipped    {}", r.skipped.len());
            println!("scan       {ms} ms");
            if show_skipped {
                for s in &r.skipped {
                    println!("  skip  {}  ({})", s.path.display(), s.reason);
                }
                for a in &r.aliases {
                    println!("  alias {}  -> {}", a.rel_path, a.target_rel_path);
                }
            }
        }
        Cmd::Search {
            common,
            query,
            limit,
            all,
        } => {
            let (vault, _) = common.open(|_| {})?;
            // A row is an occurrence, so a file says itself once per match: the line is what
            // tells two of its rows apart.
            for h in vault.search(&query.join(" "), limit, all)? {
                let at = h.line.map(|l| format!(":{l}")).unwrap_or_default();
                println!("{}{at}\n  {}", h.rel_path, h.snippet.replace('\n', " "));
            }
        }
        Cmd::Backlinks { common, rel_path } => {
            let (vault, _) = common.open(|_| {})?;
            for b in vault.backlinks(&rel_path)? {
                println!("{}  [{}..{}]", b.src_rel_path, b.byte_start, b.byte_end);
            }
        }
        Cmd::Tags { common } => {
            let (vault, _) = common.open(|_| {})?;
            for (name, count) in vault.tags()? {
                println!("{count:>6}  #{name}");
            }
        }
        Cmd::Stats { common } => {
            let (vault, _) = common.open(|_| {})?;
            println!("{}", serde_json::to_string_pretty(&vault.stats()?)?);
        }
        Cmd::Serve { vault, db } => {
            // Logging must not go anywhere near stdout: that is the protocol. Stdin unlocked,
            // because the server reads it on a thread of its own.
            accent_api::rpc::serve(&vault, db.as_deref(), std::io::stdin(), std::io::stdout())?;
        }
        Cmd::GitRpc => {
            accent_api::git_remote::serve_once(std::io::stdin().lock(), std::io::stdout().lock())?
        }
        // Logging goes to stderr: stdout is the protocol.
        Cmd::Mcp { common, read_only } => mcp::run(&common.vault, &common.db_path(), read_only)?,
        Cmd::Hold => hold::daemon::run()?,
        Cmd::Attach { cwd, owner, id } => {
            std::process::exit(hold::client::attach(&id, cwd, &owner))
        }
        Cmd::Kill { id } => hold::client::kill(&id)?,
        Cmd::Held => hold::client::held()?,
        Cmd::Clip { id } => hold::clip::take(&id)?,
    }
    Ok(())
}
