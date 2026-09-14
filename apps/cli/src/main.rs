//! accent-cli: index | scan | search | backlinks | tags | stats. Works without the GUI.
//! Read-only with respect to the vault — the only thing it writes is the cache db.

use accent_core::index::{Index, Phase, Progress, default_db_path};
use accent_core::walk::{self, ScanOptions};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Parser)]
#[command(
    name = "accent-cli",
    version,
    about = "accent vault index and query CLI"
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
    /// Also leave out gitignored *files* inside the vault tree (off by default: vaults often
    /// gitignore *.md). A gitignored directory is left out either way.
    #[arg(long)]
    vault_gitignore: bool,
    /// Do not honour .gitignore inside directory-symlink targets (on by default, so that
    /// external code repos do not drag .venv / target / node_modules into the index).
    #[arg(long)]
    no_target_gitignore: bool,
    /// Index dependency and build trees too. Off by default: a directory carrying CACHEDIR.TAG
    /// or pyvenv.cfg is somebody's cache, not notes. This is the way back in.
    #[arg(long)]
    index_dependency_trees: bool,
}

impl Common {
    fn db_path(&self) -> PathBuf {
        self.db
            .clone()
            .unwrap_or_else(|| default_db_path(&self.vault))
    }
    fn scan_options(&self) -> ScanOptions {
        ScanOptions {
            vault_gitignore: self.vault_gitignore,
            target_gitignore: !self.no_target_gitignore,
            skip_dependency_trees: !self.index_dependency_trees,
            ..ScanOptions::default()
        }
    }
    fn open(&self) -> Result<Index> {
        let p = self.db_path();
        Index::open(&p).with_context(|| format!("opening index at {}", p.display()))
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
    /// Full-text search over note bodies.
    Search {
        #[command(flatten)]
        common: Common,
        query: Vec<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Notes linking to a note.
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
            let mut ix = common.open()?;
            let t = Instant::now();
            let stats =
                ix.reconcile_with(&common.vault, &common.scan_options(), |_, p: Progress| {
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
            let r = walk::scan(&common.vault, &common.scan_options());
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
        } => {
            let ix = common.open()?;
            // Everything, always: the CLI is a diagnostic tool with no All toggle to offer, and
            // the ignore set is only ever written by the desktop app's git refresh.
            for h in ix.search(&query.join(" "), limit, true)? {
                println!("{}\n  {}", h.rel_path, h.snippet.replace('\n', " "));
            }
        }
        Cmd::Backlinks { common, rel_path } => {
            let ix = common.open()?;
            for b in ix.backlinks(&rel_path)? {
                println!("{}  [{}..{}]", b.src_rel_path, b.byte_start, b.byte_end);
            }
        }
        Cmd::Tags { common } => {
            let ix = common.open()?;
            for (name, count) in ix.tags()? {
                println!("{count:>6}  #{name}");
            }
        }
        Cmd::Stats { common } => {
            let ix = common.open()?;
            let s = ix.stats()?;
            println!("{}", serde_json::to_string_pretty(&s)?);
            println!("unresolved links {}", ix.unresolved_links()?.len());
        }
        Cmd::Serve { vault, db } => {
            // Logging must not go anywhere near stdout: that is the protocol. Stdin unlocked,
            // because the server reads it on a thread of its own.
            accent_api::rpc::serve(&vault, db.as_deref(), std::io::stdin(), std::io::stdout())?;
        }
    }
    Ok(())
}
