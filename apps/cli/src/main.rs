//! accent-cli: index | scan | search | backlinks | tags | stats. Works without the GUI.
//! Read-only with respect to the vault — the only thing it writes is the cache db, which it brings
//! up to date through the same façade, scan policy and defaults as the app before it answers.
//! Also what runs behind the window: `serve` on a remote host, and `hold`, `attach`, `kill`,
//! `held` and `clip`, which keep the terminal tabs' shells alive between windows.

mod hold;

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
    /// Index database. Defaults to $XDG_CACHE_HOME/accent/<hash-of-vault-path>.db
    #[arg(long)]
    db: Option<PathBuf>,
}

impl Common {
    fn db_path(&self) -> PathBuf {
        self.db
            .clone()
            .unwrap_or_else(|| default_db_path(&self.vault))
    }

    /// Open the vault as the app does and wait for the walk that brings its index up to date,
    /// telling `progress` how it goes. Unwatched: the command is over before a change could
    /// matter.
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
        /// Print per-batch progress.
        #[arg(long)]
        progress: bool,
    },
    /// Walk the vault without touching the database (scan-only timing).
    Scan {
        #[command(flatten)]
        common: Common,
        /// List every path the symlink rules rejected.
        #[arg(long)]
        show_skipped: bool,
    },
    /// Full-text search over the indexed files.
    Search {
        #[command(flatten)]
        common: Common,
        query: Vec<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Also search the files git ignores, which are left out by default as in the Search pane.
        #[arg(long)]
        all: bool,
    },
    /// Notes linking to a file.
    Backlinks {
        #[command(flatten)]
        common: Common,
        rel_path: String,
    },
    /// Every tag with its use count.
    Tags {
        #[command(flatten)]
        common: Common,
    },
    /// Index contents at a glance.
    Stats {
        #[command(flatten)]
        common: Common,
    },
    /// Serve one vault as JSON-RPC over stdin and stdout.
    ///
    /// This is what runs on a remote host: the desktop uploads this binary, starts it over ssh,
    /// and drives the whole façade through the pipe. It answers until stdin closes, which is what
    /// happens when the window goes away, or until a client that pinged has said nothing for 90 s,
    /// which is what a dropped link looks like from here. Nothing is left behind on the host.
    Serve {
        /// Vault root directory.
        #[arg(long)]
        vault: PathBuf,
        /// Index database. Defaults to $XDG_CACHE_HOME/accent/<hash-of-vault-path>.db
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Hold the terminals' shells: the daemon `attach` starts when none is running.
    ///
    /// One per user and machine. It leaves on its own when its last shell has ended, or ten
    /// seconds after it was last asked anything when it never held one, and returns at once when
    /// another holder is already up.
    Hold,
    /// Attach this terminal to held shell ID, starting the holder and the shell if need be.
    ///
    /// What a terminal tab runs, locally or over `ssh -t`. Closing the terminal only detaches:
    /// the shell stays with the holder until it exits or is killed. Exits with the shell's status
    /// when it ends, 0 when detached or taken over by another terminal, and 254 when it could not
    /// attach at all (never 255, which is ssh's own).
    Attach {
        /// Where a new shell starts. Defaults to the current directory.
        #[arg(long)]
        cwd: Option<PathBuf>,
        id: String,
    },
    /// End a held shell and everything running in it.
    ///
    /// An id no shell has yet is remembered, so that an attach still on its way with it starts
    /// nothing; a holder is started to remember it if none is running.
    Kill { id: String },
    /// List the held shells, one JSON object per line.
    Held,
    /// Print the last copy (OSC 52) held shell ID made, as base64, and forget it.
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
        Cmd::Hold => hold::daemon::run()?,
        Cmd::Attach { cwd, id } => std::process::exit(hold::client::attach(&id, cwd)),
        Cmd::Kill { id } => hold::client::kill(&id)?,
        Cmd::Held => hold::client::held()?,
        Cmd::Clip { id } => hold::clip::take(&id)?,
    }
    Ok(())
}
