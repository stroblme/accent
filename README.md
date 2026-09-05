# Accent

This is an opinionated text editor which can serve as a knowledge management system and IDE in one software.
It aims to become a combination between Obsidian and VSCode with focus on minimal, distraction free working.
While both examples are great apps, they caused me some frustration in the past which led me to building my own solution:
- slow startup time in Obsidian making it infeasible to use for "quickly" editing that single file
- closed source spirit of Obsidian
- VSCode being maintained by Microsoft and remote ssh being proprietary code
- VSCode not feeling "native" on linux/gnome
- VSCode having one of the worst pdf reader capabilities I've ever seen

Based on these experiences, I thought of some requirements:
- referencing/linking between files and support for tags in markdown
- ability to use templates and commands
- startup in <1s independent of the size of the vault
- support for mobile
- no proprietary software pieces / everything open source
- ability to view and annotate PDFs
- syntax highlighting, debugging and remote ssh capabilities
- distraction free interface and native look and feel
- git viewer/ git management
- integrated terminal support
- MCP support

## Getting Started

```
cargo build            # core + api + cli
cargo test
cargo build -p accent  # GTK app
cargo run --release -p accent -- testvault
```

## Architecture (decided 2026-09-03)

- **One Rust core** (`crates/core`): SQLite/FTS5 index as a disposable cache over plain markdown files, Syncthing-safe atomic saves, symlink-aware reconcile, markdown → styling spans/links/tags, PDF via pdfium.
- **Linux UI**: GTK4 + libadwaita in Rust (`apps/gtk`), GtkSourceView 5 editor styled from core spans, WebKitGTK preview. The only stack that is truly native on GNOME Wayland.
- **Android UI**: Kotlin + Compose + Material 3 over uniffi bindings of the same core (Phase 3).
- **CLI / MCP**: `accent-cli` renders the same core as JSON-RPC over stdio; `accent-cli mcp` works with the app closed.
- **Roadmap**: desktop MVP → PDF + MCP → Android → IDE features (terminal, git, LSP, remote SSH, DAP).
- Licence: GPL-3.0-or-later.