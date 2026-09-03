# Accent

This is an opinionated

- markdown editor
- knowledge management system
- pdf viewer

cross-platform (without compromises) and blazing fast.

## Architecture (decided 2026-09-03)

- **One Rust core** (`crates/core`): SQLite/FTS5 index as a disposable cache over plain markdown files, Syncthing-safe atomic saves, symlink-aware reconcile, markdown → styling spans/links/tags, PDF via pdfium.
- **Linux UI**: GTK4 + libadwaita in Rust (`apps/gtk`), GtkSourceView 5 editor styled from core spans, WebKitGTK preview. The only stack that is truly native on GNOME Wayland.
- **Android UI**: Kotlin + Compose + Material 3 over uniffi bindings of the same core (Phase 3).
- **CLI / MCP**: `accent-cli` renders the same core as JSON-RPC over stdio; `accent-cli mcp` works with the app closed.
- **Roadmap**: desktop MVP → PDF + MCP → Android → IDE features (terminal, git, LSP, remote SSH, DAP).
- Licence: GPL-3.0-or-later.

## Idea

I'm thinking of building a custom editor "accent". I've been using Obsidian for years now but I get frustrated with it's inability to handle large sets of files and the resulting slow startup time. 
I've been using apostrophe recently and really like the experience but of course it lacks a file navigation, linking and all the other neat features of obsidian. 
Furthermore, I would like to have the same experience on mobile (android) and desktpo (linux). 
I'm currently thinking of a flutter-based app, but also sth. web-based would be feasible (and then use e.g. Tauri); this would allow for more flexibility concerning UI/UX.
I would like to collaboratively plan this app. Following is a list of features I would like to have in "accent":
- ability to edit and preview markdown files (ui as apostrophe)
- ability view and annotate pdfs (obsidian has the ability to select text in a pdf and put a reference to this highlighting in a markdown file) with pdf following the app theme (I developed UNote a while ago, so have some experience with pdf annotations)
-  ability to reference documents and having tags (obisidan style)
-  file/folder management with the ability to handle symbolic links (I think this is what currently breaks my obsidian instance) in conjunction with safe read/write file operations (like vscode) as I'm using syncthing to synchronize my vault which can cause conflicts if files come in mid-edit
-  sub-second startup, independent of the vault size
-  mcp server for interacting/chatting with the vault via AI
-  command palette + file switcher (like vscode/obsidian)
-  ability to use templates and commands (like obsidian) to generate meeting note templates or daily notes
-  file tabs to quickly switch between multiple open files

In the long run, I think of replacing "vscode" with this app; there is not much I would need in terms of making "accent" an IDE-feeling:
- different modes for "writing" and "coding" (could switch automatically per workspace or file opened)
- remote ssh (this is the most important feature)
- debugging capability (this will likely be the hardest feature)
- git viewer (including submodules) and git tree view
- terminal