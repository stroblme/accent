# accent design guidelines

The rules for the GTK4 + libadwaita desktop app and the decisions under it; MOBILE_DESIGN.md holds Android's, and the Material 3 mapping says which Compose widget stands in for which desktop one. The precedents are Apostrophe and GNOME Text Editor: the note fills the window, the chrome stays out of the way, nothing is decorative. Where this document is silent the GNOME Human Interface Guidelines decide (<https://developer.gnome.org/hig/>, <https://developer.gnome.org/hig/principles.html>).

## Principles

1. The note is the UI. Everything else is scaffolding that earns its pixels or disappears.
2. One accent colour, taken from the system. No second brand colour, no decorative chrome.
3. Every action is reachable from the keyboard, and the palette is the discoverable index of them.
4. Light and dark are the same design, not two designs. Same for the seven system accents.
5. The system owns fonts, colours, animation and scaling. We only own layout and behaviour.

## Architecture

The decisions under the code, each with the reason it was taken. Android's own are in MOBILE_DESIGN.md.

- **Stack**: GTK4 + libadwaita in Rust on Linux, Kotlin + Compose on Android, one Rust core — the only pair native on both; every other stack draws a replica of GNOME's chrome or ignores its dark mode and accent. The desktop links the core directly; Android reaches it through uniffi.
- **Crates**: `crates/core` holds no UI type; `crates/api` is the façade every client goes through — a `Vault` over a local or a remote backend, plain serde types, JSON-RPC over stdio for `accent-cli`, the tests and `serve`, uniffi behind the `android` feature; `crates/drawio` depends on neither. Annotate the façade, never the core (Anytype's and Zed's shape).
- **Plain files are the truth**: the index is a disposable cache, rebuilt on any schema bump, and nothing but the user's own files is written into a vault — config in `~/.config/accent/config.toml`, sessions in `~/.local/state/accent/<hash>.json` — so Syncthing never carries UI state between devices.
- **Never block the UI** on the index, a save or the network: a vault's index and watcher run on one worker thread and report through events, and the window paints from the stored index while the reconcile runs behind it — which is why startup does not depend on vault size.
- **Index**: SQLite (bundled rusqlite, FTS5, WAL) — the in-memory indexes of Obsidian, Zettlr and Foam are what makes a large vault slow to open.
- **Index writes**: every write transaction is `BEGIN IMMEDIATE` (`Index::write_tx`) — a deferred one that reads before it writes gets SQLITE_BUSY without the busy handler ever running, which is the "database is locked" an autosave's reindex raised against the git refresh's write.
- **Saves**: `canonicalize` → temp file in the same directory → fsync → mode, uid and gid kept → rename, gated on an etag `(mtime_ns, size, ino)` — safe under Syncthing, and never over a change nobody has seen (States, Saving never answers a question).
- **Syncthing**: `*.sync-conflict-*` files are never notes and get the resolve UI; symlinks dedup by `(dev, ino)`, loops and in-vault targets are rejected — conflict copies reach every device, and a vault links external directories in on purpose.
- **Watch set**: one non-recursive inotify watch per directory the walk kept, rebuilt when a directory appears — a recursive watch re-adds every skipped tree, and the walk's own directory opens overflow `max_queued_events` into a rescan that loops.
- **Delete** is the system trash (`gio::File::trash`) — Syncthing propagates the deletion and Files restores it; on a remote vault it is permanent behind a dialog, the static server having no trash portal.
- **Moves keep their links**: a rename or move rewrites every wikilink, embed, markdown link, reference definition and HTML `src`/`href` it would leave naming the wrong place, in the notes pointing at what moved and in the moved notes, after one Update Links? question naming only the notes that change. One question per link: from its note's place after the move, does it still find the file it found before? A wikilink resolves by name, so it changes only when its key no longer names the file and keeps its author's spelling; a markdown link is a path, so it becomes the percent-encoded relative path from its note's new folder; nothing the index did not resolve to a vault file before the move is touched.
- **Remote vaults** follow Zed: the system `ssh` binary with one ControlMaster per vault (`ControlMaster=auto`), and a musl-static `accent-cli serve` uploaded under its own blake3 hash and driven by JSON-RPC over the session's stdio; index, watcher, search, git, language servers and shells run on the host — `~/.ssh/config`, ProxyJump and the agent stay ssh's job, the hash is the version, and a static binary starts on any distribution. sshfs needs no code: a mount is a local vault.
- **Remote prompts**: `SSH_ASKPASS` points back at accent, which runs as its own dialog under `ACCENT_ASKPASS` — a passphrase or a host key reaches a window, not a terminal nobody watches.
- **Remote bytes** never travel in the protocol: a reader that needs a real file `fetch`es it over the master into a cache keyed by etag — base64 through a JSON parser is neither fast nor debuggable.
- **Remote lifetime**: one `serve` per window, ending on stdin EOF or after 90 s without a ping once it has been pinged; closing stops it and cancels the forwards but leaves the master its 60 s ControlPersist — no zombies after a link that dies without closing, and a reopen within the minute skips the handshake and the passphrase.
- **Remote session**: stays on this machine, keyed by the `ssh://` address — where the windows and tabs were is a fact about the desk, not about the files.
- **Git**: shell out to the `git` binary (Zed's choice) — least code, and hooks and credential helpers work. The repositories are the vault root's plus every indexed directory holding a `.git` entry — a stat per directory the index already lists, not a second walk. The vault worker watches each `.git` and its `refs/heads` on both backends (`Event::GitChanged`) — the one mechanism that also works on a host.
- **Git processes** run in a session of their own (`setsid`) and are stopped as a group, and a dead link is given up after 15 s (ssh `ServerAlive` 5 s × 3, TCP keepalive for http, unless the user's own ssh or `http.keepAlive*` says otherwise) — a stopped git removes its lock, and a vanished remote does not hang the pane.
- **Held shells**: every shell runs under `accent-cli hold`, a daemon of accent's own keeping a `vt100` model of the screen, reached by a dtach-style `attach` in VTE's pty, locally and over ssh alike — a layout-only restore loses the running build, and tmux would put its prefix key in every tab and have to be on every host. Its socket is `$TMPDIR/accent-<uid>/hold-1.sock`, versioned — `$XDG_RUNTIME_DIR` goes at a full logout, and the version keeps a new `attach` off an older holder.
- **Language client**: our own tokio client in `crates/lsp`, protocol types transcribed from `lsp-types` (MIT), one process-wide runtime — `lsp-types` is unmaintained and `async-lsp` pins an old one, and `accent-core` stays runtime-free for Android.
- **Language API**: one `Language` trait over a real server and the index, async `Vault` methods returning a `Task` that cancels at the server when dropped, positions in characters — every text tab takes one path, and a completion typed past is not answered into the void.
- **Notes as a language**: the index answers for markdown through that trait — `[[` and `#` completion, hover, definition, headings as symbols, backlinks as references, dangling links as hints, sections and fences as folds — one code path in the UI, and on a remote vault it runs on the host beside the files.
- **Word suggestions**: prose (markdown, LaTeX, BibTeX, reST, plain text) gets a second completion source under the first: the document's own words, most used first, then the system hunspell stems, 40 at most — the text's own words are the likeliest next ones, and hunspell's `.dic` is already installed for libspelling.
- **Ghost text** is LSP `textDocument/inlineCompletion` from `merl-rt`, painted by the view and never put in the buffer — any server answering the standard method works, and a suggestion costs no undo step, save or reparse. merl hears `didSave` when a document is left, not on each autosave, because it re-reads the whole vault on one.
- **Markdown styling** is computed in core as byte-range spans both apps apply, with the markup visible and styled beside a separate rendered preview, as in Apostrophe — one styling implementation; hiding the markup would be GtkTextTag work of its own.
- **Syntax highlighting** is GtkSourceView's language specs and style scheme, not core spans — 180 languages for no code; syntect in core is the path the day Android needs code.
- **No modes**: behaviour follows the file type, not a writing/coding switch — a mode is a setting to get wrong, and the file already says what it is.
- **PDF engine**: pdfium-render in core over a libpdfium fetched by `make pdfium`, annotations written through its own API rather than `lopdf` — one BSD engine on both platforms with per-glyph text, and nothing new to vendor. Without libpdfium a PDF tab is a status page and the build still passes.
- **PDF highlights live in the notes**: an Obsidian-compatible `[[f.pdf#page=N&selection=a,b,c,d|quote]]` link, found through the index's `links` table and its `alias` column, with no annotation table of its own; export to real `/Highlight` annotations is optional — rewriting the PDF per highlight is Syncthing conflicts and binary churn (Zotero's reasoning), and only what the notes hold survives an index rebuild.
- **Ink geometry** lives in the annotation's appearance stream, not in `/InkList` — every viewer renders the stream, and pdfium-render does not wrap `FPDFAnnot_AddInkStroke`.
- **Diagrams**: `crates/drawio` ports mxGraph's model, routing, shapes and label placement faithfully (the Apache-2.0 parts attributed file by file, with `LICENSE-APACHE` and `NOTICE`), keeps every attribute it does not interpret, and hands the app a UI-neutral display list — a file saved here opens in draw.io as it was, the port can be diffed against later draw.io releases, and Android can paint the same list. A formula label is typeset by WebKit, as draw.io's MathJax does.
- **MCP** is `accent-cli mcp` over stdio on the same core, not a server inside the app — it keeps working with the app closed, which no Obsidian MCP server does (not built yet: NOTEPAD).
- **Licence**: GPL-3.0-or-later — no commercial intent, GNOME's own licence, free to borrow from Papers and Apostrophe, and compatible with pdfium's BSD.

## Layout map

<https://developer.gnome.org/hig/patterns/containers/header-bars.html> · <https://developer.gnome.org/hig/patterns/nav/sidebars.html>

### Window shell

- `AdwApplicationWindow` > `GtkPaned`, one `AdwToolbarView` per side, each with its own `AdwHeaderBar`.

### Header bars

- Two, so the sidebar reaches the top of the window and the tab bar spans only the editor column.
- Sidebar header: the start window controls and the pane switcher.
- Main header: the sidebar toggle (always visible, so a hidden sidebar comes back without the keyboard), `AdwWindowTitle` with the vault name and the note path, the Drawing toggle while a PDF or a diagram is in front, the primary menu, the end window controls. It takes the start window controls while the sidebar is hidden.
- Undo and Redo sit left of the Drawing toggle while a tool is in hand and either has something to walk (over a diagram, whenever either has), as a pair with the empty one insensitive, so neither moves under the pointer.
- Toggle Preview has no button, only its action, chord and palette entry: a window has only so many places for one.
- A header carries the document's name, not its facts; those, and what the window is busy with, are the status bar's.
- The window's own title is the vault name alone — `Notes (host)` on a remote vault, `Accent` without one — and does not follow the tab: it is what tells windows apart in the switcher and the task manager.

### Sidebar

- An `AdwInlineViewSwitcher` in icon mode over an `AdwViewStack` of eight panes: Files, Search, Tags, References, Git, Ports, Outline, Properties. The switcher sits in the sidebar header, level with the sidebar toggle, so the tree's first row lines up with the tabs.
- Git shows only where there is a repository, Ports only on a remote vault, Properties only while a diagram is in front: each only where it has something to say, so other vaults keep the switcher they had.
- A vault always opens on Files: a search from another sitting is not where a vault starts.
- Width is dragged on the `GtkPaned` handle, floor 200.
- Below 760 sp an `AdwBreakpoint` hides the sidebar: that is the default 280 px sidebar beside the 480 px column floor. Above it the sidebar comes back as it was, at its dragged width.
- `F9` still shows the sidebar in a narrow window, beside the note rather than over it.
- Neither the narrow-window collapse nor presentation mode is session state: the session saves the sidebar as it was before either.

**Files**

- The vault's name sits above the tree as a plain label with a folder icon: it is what the listing is of, and a root drop zone that stays on screen however deep the tree is scrolled.
- Every folder is listed, the ones the index does not walk included (`node_modules`, a `.venv`, a cargo `target/`): a listing that silently leaves a folder out cannot be trusted. Those rows are read off the disk one level per expansion, stay out of the index, the watcher and every query, and are dimmed, search not reaching them.
- A dependency tree (`node_modules`, a `.venv`, a marked `target/`) lists and opens but is never changed — no menu, marking, drag or drop: it is somebody else's. A gitignored folder is the reader's own and gets the whole menu: being out of the index stops a file being searched, not edited.
- A gitignored folder is watched while the tree holds its listing (local vaults only); a dependency tree is not — 40 000 files is the tree that must not be watched — and is as fresh as its last expansion.
- `.git` and `.trash` are out of reach here as everywhere. A remote vault lists the same way, the merge happening on the host.
- Dot-named files and folders are listed, dimmed: a vault holds dotfiles people edit, and they are still hidden files. Show Hidden Files, on by default, ends every tree menu and is a palette command with no chord (`Ctrl+H` is Replace); off, it leaves out every dot-named path the index holds, while the skipped trees still list, `.venv` included. It is a preference, so every window follows it.
- The selection follows the open tab, and the open file's row is put back when the pointer leaves the list: GTK's single-click activation also selects on hover. Only among rows on screen: no folder opens and nothing scrolls, which is Reveal in Sidebar's job.
- Ctrl+click and Shift+click mark rows, the tree's only multiple selection, tinted with the accent: the selection is already the hover highlight and the open file. Shift+click marks the rows on screen from the last click without Shift and replaces the marks; Shift+Ctrl+click adds the range. A shut folder in a range is marked whole, and a Ctrl+click on a row inside it takes that row alone out. Neither click opens or toggles anything; a plain click (once it is not a drag) and Escape let the set go. A row the index does not hold is never marked. There is no keyboard multi-select.
- A right-click on a marked row is about the whole set and offers only Cut, Copy, Move to Trash, Paste and Show Hidden Files (Principle 1); a right-click elsewhere forgets the marks. Delete trashes the set, a marked row dragged carries it, and a set acts on its top-most paths. A move or a trash lets the marks go, since it may have taken what they named.
- Files is a drag surface both ways: a row dropped into a pane opens there, on a pane's edge splits it; dropped in the tree it moves — onto a folder into it, onto a file beside it, onto the vault label or the blank area below the rows into the root. Folders drag too, but can never be dropped into a pane and opened as a note.
- A marked set dragged lands as one move: one plan, one Update Links? question, and a name the destination already holds refuses all of it.
- A move that cannot happen (a folder onto itself or into what is under it, anything into the folder it is in) is refused while the pointer is over the row, which does not light up; GTK's `:drop(active)` outline is the whole feedback, with no insertion line. Moves go through Rename's plan-and-rewrite, so open tabs follow the file and links keep their confirmation.
- Files dropped from another application (GNOME Files' `GdkFileList`) are copied in as a paste of the file manager's clipboard is — the same `(copy)` mark, the same upload on a remote vault; a drop offering move alone (Shift in the file manager) moves.
- A drag resting over a shut folder opens it after a second, so a file reaches a folder that was not on screen; nothing shuts it again.

**Search**

- The entry on a line of its own; under it a `.linked` group of Match Case / Match Whole Word / Regular Expression / All toggles with the replace toggle at the row's far end; a revealer with the replace field; a progress bar above the results.
- The four toggles are text (`Aa`, `Word`, `.*`, `All`): Adwaita has no glyph for them. The replace toggle is `edit-find-replace-symbolic`, outside the group: it opens a panel rather than changing the query.
- The search space is everything the index holds — every note and every other text file under 1 MiB — minus what git ignores.
- `All` drops that exclusion in either mode, and reaches the trees the walk never entered in an exact search only: those need a walk per query. Its tooltip names both halves; forcing exact mode would turn a ranked query into a literal substring and quietly find fewer files.
- Replace All rewrites every file the index holds a body for, source files included, so its button counts every match the index lists, uncapped. Rows `All` adds past the index get no replacement preview: the rewrite never opens them.
- A note is never excluded, whatever ignores it: a notes vault gitignoring its own `*.md` is ordinary, so every filter has a markdown escape, and the tree dims an ignored file rather than hiding it.
- There is no ignore file in the vault: `[search] exclude` in `config.toml` (vault-relative directories) joins git's answer in the same exclusion, so every rule above holds for it and a vault without a repository still has one. It does not replace the walk's built-in skips, which are about what the index and the watch budget can hold, not what a reader wants to see.

**Tags**

- A vertical `GtkPaned`: the tag list above, the files carrying the selected tag in the lower third.

**References**

- On a note, the notes linking to it, once each. On a source file, every use of the symbol under the caret as `path:line`, following the caret while shown. Wherever that would be nothing (a PDF, an image, a diagram, a text file with no server, the caret on nothing the server knows), the notes linking to the file.

**Outline**

- What the open tab outlines: a text file's symbols from the language layer (a note's headings, a source file's functions and types, nested as the server nests them), a PDF's bookmarks and thumbnail strip (see PDF), or an empty state.
- On a text tab it follows the caret, as VS Code's Follow Cursor does: the innermost heading or symbol holding the caret is selected and scrolled into view as little as it takes, never activated and never given the keyboard. Above the first heading the list returns to its top; in a gap between two functions it stays put, or it would jump to the top and back. An edit refills it in place, and a tab switched back to opens on its caret's section. The caret's row is put back when the pointer leaves. There is no toggle.

**Properties**

- The diagram's own widget: `AdwPreferencesGroup` rows for the selected cells — Shape (fill and line colour, each with a switch for having one; line width, dashed, rounded, shadow, opacity), Text (size, colour, bold/italic/underline, alignment), Line for an edge (route, arrow heads), Style (one cell's raw style string, applied on Enter) — and with nothing selected, the page (name, background, size).
- Every row is one undo step; spin rows wait for a burst of steps to settle.
- GTK's own colour chooser is the whole picker: it cannot be given a palette, so none is imitated.
- The pane leaves by showing Outline first, so the stack never shows nothing. Its icon is `document-properties-symbolic`.

**Ports**

- The forwards over the vault's connection, a row each — `8080 → 80`, the local port on the left either way, both ends in the tooltip, a stop button — above two port boxes with the direction between them as a flat button: `→` a local port reaching the remote one (`ssh -L`), `←` a remote port reaching this machine (`ssh -R`, on the host's loopback).
- The direction is kept across Adds; a row's own is read-only, a forward the other way round being a new forward.
- ssh refusing a port is a banner over the list: it is the state of a forward that is not up.
- The connection restores the list after a reconnect; nothing outlives the vault.

**Result lists**

- Every result list activates on a single click, as the tree and the Git pane's changed files do: one click opens what a row stands for, as in GNOME's own sidebars. It opens the pane's preview tab (Tabs), so walking a list replaces one tab instead of leaving twenty.
- A search result row is a match, not a file: each quotes its line with the match marked and the file and line dim beside it, and past five rows from one file the rest gather into a "+N more in this file" row, so no file takes the list. A hit opens on its place with the match marked.
- The marks are the tab's, not the bar's: they last until an edit moves them or that pane's bar replaces or clears the query.

### Git pane

- One repository at a time, from a `GtkDropDown` that hides itself when there is only one and ellipsizes the name, which would otherwise set how narrow the sidebar can be dragged.
- A branch row: a `GtkMenuButton` labelled with HEAD's branch, or `Detached at abc1234` (the status bar's string). Its popover lists the local branches, then under a dim Remote heading the remote-tracking branches no local branch shares a name with, then Create Branch… and Merge Branch…: the readout is every branch operation the pane offers.
- Picking a row is `git switch`, a remote row `git switch --track`; whether it is safe is git's call, its first line coming back as a toast and the label returning to the real HEAD where git refuses.
- Every row but the checked-out one carries a trash button on hover: git refuses to delete HEAD's branch, and a control that cannot work is dead chrome. Deleting is `git branch -d`, escalating to `-D` only after an `AdwAlertDialog` naming what would be lost — no guess of ours at a default branch. Delete Branch… in the palette is the same by keyboard. A remote row has no trash: deleting a remote branch is a push, a terminal job.
- Create Branch… is `git switch -c` from HEAD, with no base picker and git validating the name.
- Merge Branch… merges another local branch into HEAD with no fast-forward flag, so git's default and the user's `merge.ff` decide. The toast says what came of it — up to date, fast-forwarded, merged, or conflicts in N files — read off the repository, not off git's words.
- A stopped merge raises an `AdwBanner` over the pane, "A merge is in progress", until committed or aborted; its Abort asks first, since it discards every resolution made so far. A stopped rebase raises the same banner reading "A rebase is in progress": Commit reads Continue and the message box goes, each commit keeping its own message, and Abort asks the same question. A Continue that stops again leaves the banner up.
- Conflicts are the Merge Conflicts section's rows: a row opens the note with git's markers, Stage marks it resolved, and nothing opens on its own.
- Sync and Commit share a row, the pane's two actions: a row of its own costs 40 px the changes and the history need. Sync carries the ahead and behind counts and `mail-send-receive-symbolic`.
- Sync pulls then pushes, both halves every time: the counts come from a background fetch (on opening the vault, on picking a repository, every five minutes while the window has focus), so they are a readout, not a decision. A failed pull stops there and keeps its transcript.
- A failed background fetch interrupts nobody and says so on the Sync tooltip. The fetch and a Sync never run together, both writing the remote-tracking refs: the fetch skips a tick that lands on a Sync, and a Sync asked for mid-fetch waits for it, 8 s at most.
- With no upstream the button's tooltip says Publish: it pushes the branch to its remote (the only one, or `origin`) and tracks it, instead of answering with git's "There is no tracking information".
- There is no Refresh button: a save, a watcher event and a `.git` write each schedule one — a write, never a read, or the pane's own reads would re-trigger it forever. A `git init` in a vault with no repository is such a write, so the pane shows itself unasked.
- The commit message box takes `Ctrl+Return` while it has the keyboard, although the chord is Insert Line Below: the action offers it to the box first.
- Commit commits what is staged, or `git commit -a` where nothing is (tracked changes in, untracked files out, as VS Code does with an empty index). Never `-a` during a merge, where it would stage files with their markers: the box reads Merge message (optional), Commit is live with nothing staged, and an empty box commits git's own message with its `# Conflicts:` list stripped. While a conflict is unstaged, Commit is insensitive, tooltipped Stage the resolved conflicts first, and `Ctrl+Return` does nothing: git would only refuse.
- The box is one line at rest and grows with its text to eight lines, then scrolls: a message has a body, but must not push the changes and the history off the pane. Its text inset is Adwaita's `entry` one, like the search field's.
- The box and the button disappear when there is nothing to commit, but never while the box holds text or the keyboard: a refresh fires on every save and would take a half-written message with it.
- Below, a `GtkListView` of sections — Merge Conflicts, Staged Changes, Changes, Submodules — with empty sections not drawn.
- Files are grouped by folder by default, as the tree sorts them, a chain of single-child folders on one row (`src/deep`, as in VS Code): a flat list repeats one directory per change. Under a folder row a file drops its directory label, and every file row clears the chevron column so its icon lines up under a sibling folder's. The grouping is one preference, `git_tree`, a switch in Preferences, not per-window state.
- A row's Stage / Unstage / Discard buttons show on hover and on `:focus-within`, so the keyboard reaches what the pointer does. Discard asks in an `AdwAlertDialog`, the one action here that can lose work. A folder row acts on everything under it in its section (not in Merge Conflicts, where a conflict is resolved one file at a time), and its Discard says how many files go back to the index and how many untracked ones to the trash.
- A single click on a row opens its diff as a tab. A file deleted from the working tree opens as the index against nothing.
- A row whose file was staged, discarded or committed since the refresh opens nothing, two identical columns being no answer: the pane says the file has no unstaged changes and asks git again, which takes the row away.
- `Ctrl+Shift+G` shows the pane with the keyboard in the commit box.
- The lower half, across a vertical `GtkPaned`, is the history: 200 commits at a time behind Load More, each in its lane's colour (the hue rotation, Colour). A commit expands on a single click into the files it changed, each opening the commit against its first parent as a diff tab.
- A commit leads its summary with a label per decoration — HEAD's branch in the accent, a remote branch dimmer, a tag outlined — three at most and `+N` for the rest; a branch and a tag of one name stay apart, and `origin/HEAD`, the stash and notes are not shown.
- Each lane is named after its first decorated commit and handed down first parents, and a commit where other named lanes end says "side branched here". That is the graph's own reading, with no git call of its own, so a branch that has not diverged shares its lane and forks nowhere.
- A commit row carries Check Out Commit and Copy Commit ID on hover and `:focus-within`, so the summary has the pane's width otherwise; two actions are two buttons, not a menu. Check Out is `git switch --detach`, unasked, git refusing it where work would be lost; moving onto a branch is the popover's job.
- Hovering a commit shows its decorations, short id, lane ("On main") and whole message, from what the log already fetched.
- One commit is expanded at a time, and a refresh closes it only where the history moved, so an expanded commit and what Load More added survive the save that scheduled the refresh.

### Editor

- `sourceview5::View` in an `AdwClampScrollable` in a `GtkScrolledWindow`: a plain `AdwClamp` makes GTK insert a `GtkViewport`, whose dead adjustments break the view's own scrolling to the caret.
- A sticky block title over the top of the view pins the opening line of the heading or fenced block the first visible line is in, as VS Code pins a function signature, until that line is back on screen; the innermost block wins. Only those two blocks, both already tagged by the styling pass: a language's own structure would need a parser. A plain label in the document font, aligned with the text, on a `.view` box with a rule under it; never in a code or CSV tab.

### Code editor

- The same view, told what it is by an `editor::Flavour` of `Note`, `Code` or `Csv`: one tab type, not two, and a code tab gets none of the markdown tags.
- Code takes a GtkSourceView language guessed from the path and the content type, so `LICENSE` and `Makefile` open as what they are; bracket matching, auto-indent, and a four-space tab that stays a real tab where a makefile needs one.
- It wraps as prose does: nothing in a document scrolls sideways. A wrapped row starts one indent level deeper than its line (VS Code's `wrappingIndent: "indent"`), so it cannot pass for the next line; a note does the same, hanging a list item's rows under its text. `Alt+Z` unwraps one tab for as long as it is open, for a generated file whose columns are the point.
- A CSV is code whose columns are coloured by the hue rotation (Colour).

### Unreadable file

- An `AdwStatusPage` in the tab itself, not a dialog or a toast: `dialog-warning-symbolic`, Binary File or File Too Large, the size, and one Show in Files button (Download… on a remote vault). The tab keeps the warning icon, so the bar says which open file would not open.

### Preview

- A read-only WebKitGTK 6 view, the same clamp width, its stylesheet generated from `AdwStyleManager`.

### Terminal

- A `vte4::Terminal` in a tab, not a panel: it splits, drags between panes and takes the tab menu, and a shell at the bottom of the window is a pane split downwards. It does not move into another window: a running shell is handed back with a toast (Tabs).
- `Ctrl+J` opens one at the vault root, `$HOME` without a vault; `Ctrl+W` closes it like any tab.
- Every shell is held by `accent-cli hold` (Architecture) and reached through `accent-cli attach <id>` in the tab's pty, `ssh -t` on a host: Close Tab ends the shell, closing the window only lets go of it, and a vault or terminal session opened again attaches to the same shell with its screen replayed.
- The tab key is `terminal:<16 hex>`, the holder's id; the shell's directory (OSC 7, else where it started) goes into the session, so a shell the holder lost (a reboot) starts again there.
- Without `accent-cli` beside `accent` the tab runs `$SHELL` directly and says so once per window. An `attach` failing on its own account exits 254 and its tab stays, so the message can be read.
- A remote vault's root that is not on the host still gets a shell, at the login's home.
- A remote shell rides a master — the vault's in a remote vault's window, one per host elsewhere — made ready once per host before the tab spawns, the tab's screen saying so. A dropped link keeps the tab, saying "Lost the connection to host. Press a key to reconnect."; a key attaches again with the screen replayed, and a vault window's reconnect does it too. Close Tab ends the shell on the host; closing the window leaves it running.
- New Remote Terminal… opens a shell on any host from any window (the Open Remote form); New Local Terminal opens one on this machine from a remote vault's window.
- A focused shell keeps every chord the Keyboard section does not reserve: a terminal that answers only half of readline is not a terminal.
- Foreground, background and all sixteen ANSI colours come from `theme.rs`, one palette per theme: VTE's own is arithmetic (its blue is 1.41:1 on our dark background), and a colour read off the widget does not follow Solarized. `vte_terminal_set_colors` takes all sixteen or none.
- GNOME's monospace font, scaled by the terminal's own zoom; an underline cursor, so the character under it stays readable.
- Inset 12 px either side, not by the 48 px page gutter: that measure is for prose, and a terminal is a grid.
- Opening one focuses the shell, so it can be typed into without a click.
- A secondary click: Copy and Paste; New Terminal and Close Tab; Open Link and Copy Link Address, the last pair only over a URL.
- `http`, `https` and `mailto` URLs in the output, OSC 8 hyperlinks included, are drawn as links and `Ctrl`+click opens one (a plain click is VTE's selection). The three schemes are an allowlist: a shell's output is untrusted, so a `file:` or `javascript:` URL is neither drawn nor launched.
- `accent --terminal [PATH]` opens a shell in a vault-less window at PATH or `$HOME` — a path that is not a directory is reported and falls back, and an `ssh://` address is a shell on that host. `accent terminal://NAME [PATH]` opens the terminal session NAME, made if new, and adds a shell at PATH.

### Status bar

- A `GtkBox` as the editor column's `AdwToolbarView` bottom bar, so presentation mode takes it with the header and a hover over its strip brings it back there (Keyboard, Views). `.caption` `.dim-label`, 6 px padding, `accent-flat`.
- At the start, what the window is busy with ("Indexing… 1200/42700 files", "Opening the document…"); at the end, the file's facts. A label with nothing to say hides.
- The vault's own work wins the first slot, being what the reader waits on; then a copy to or from a host ("Downloading a.pdf…", "Uploading 3 files…"), the latest one running, from start to toast, saying that it runs rather than how far; then "Indexing suggestions…" (the ghost-text index), a convenience that waits its turn. No language server's progress is shown: rust-analyzer reports every `cargo check`, and the line would never settle.
- The facts: what the file is (`Markdown`, `PDF`, `Image`, or `<language> · UTF-8 · LF` for code), whether it has unsaved edits, one count of its own, and its zoom.
- The count is what the tab counts: a note's words (300 ms after the last keystroke), a code tab's errors and warnings, a PDF's `Page 4 of 12` (the page under the middle of the viewport).
- The unsaved mark is the `•` a dirty tab wears: one symbol, one meaning. A saved file shows nothing.
- The branch (which syncs the document's repository) and the zoom (which resets it) are flat buttons held to the caption's line height (`.accent-bar-button`): Adwaita's button minimum would otherwise make the bar 46 px tall instead of 29.
- The zoom shown is the active tab's, never the window's document zoom over a tab it does not reach.
- A secondary press on the bar does nothing: a footer of facts is not a title bar. Button 1 still drags the window.
- The whole bar fades with the chrome while the user types, the unsaved dot included: nobody reads a footer mid-sentence.
- It speaks for the tab in front alone, so a window with no tab shows no facts.

### View modes

- Editor / Split; Split is a `GtkPaned` of the two above.

### Tabs

- One `AdwTabView` + `AdwTabBar` per pane, panes nesting in `GtkPaned`s. Each bar lives in its pane's document column (`.inline`), so it spans its own pane and presentation mode takes it with the document.
- A bar hides only while its pane is the window's only one and holds a single tab: with several panes the bar says which notes are where.
- A tab is titled with the file's whole name, `.md` included: a vault holds more than notes, and `todo` beside `todo.txt` should not be told apart by an icon.
- The icon slot is the document's own — a PDF, an image, a shell, a comparison, a file that would not open — and empty for a note or a source file: file-type icons belong to the file lists (Iconography).
- A pane is split from the tab or tree menu, `win.split-*`, or by dropping a tab or a tree row on one of its four edges. A tab dropped on the middle of another pane moves to the end of its bar, taking the keyboard. A pane whose last tab closes or leaves closes with it; the window always keeps one.
- **Reopening a vault puts its panes back**: the splits and their shares, each pane's tabs in bar order with the one in front, and the tab notes open next to. A pane that would come back empty is left out and its sibling takes the room; a tab the layout does not place joins the active pane. The window comes back in the pane the reader was in, on the tab that was in front — or, where that was a comparison, the pane's most recently used restorable tab.
- Tabs land one read at a time; a pane the reader picks a tab in, opens a note into or moves the keyboard to meanwhile is theirs, and what lands there does not change what it shows.
- A restored window has an empty Back and Forward: a restore's selections are not places. A first opening, or a session without a layout, is one pane; a split's share is kept to 10–90 %.
- A tab on a file outside the window's vault carries `document-open-symbolic` in its indicator slot, tooltipped Outside this vault, so saving it is never a surprise and it is plain why it has no backlinks.
- A tab dragged into another window moves there, its buffer written first. One vault never having two windows, it arrives from another vault and is adopted as a file from outside a vault: it edits and saves, without index, backlinks or wikilink resolution. A shell, a comparison and a remote vault's file cannot be reopened elsewhere, so they go back to their window with a toast; so does a tab let go outside every window.
- **Which tab is selected is decided by use, not position.** Each pane keeps its tabs in most-recently-selected order: `Ctrl+Tab` steps along it and `Ctrl+Shift+Tab` back, and closing a tab falls back to the most recently used one left. Per pane, because a window-wide order would move the keyboard across a split.
- While Ctrl is held each Tab steps one deeper without reordering, so three presses are three tabs back; the release commits the tab landed on, and a press past the end comes round. Nothing is shown meanwhile — VS Code's idiom without its overlay, for a chord mostly used for the last two notes. A selection from anywhere else ends the chord, so a lost release never strands the pane.
- **A tab opened by browsing is a preview** (VS Code's idiom): a click on a tree row, a search hit, a tag, a backlink, an outline heading or a Git changed file, and a wikilink followed. A pane holds at most one and the next such open replaces it, so clicking down a list leaves one tab, not twenty. What the reader named is kept: the palette, Open File…, a drop, a rename, the command line, the session.
- A preview is kept once edited, double-clicked, its eye clicked, or moved out of its pane — each the reader saying they want it. A restored tab is always kept.
- **The eye is the mark**: `view-reveal-symbolic` in the indicator slot, tooltipped Preview — Click to Keep, gone once kept. `AdwTabPage` takes no style class and no title markup, so VS Code's italics are not available and the indicator is the one slot that can speak and be clicked. Outside this vault wins the slot, saying more. A preview alone in a pane has no bar to double-click, which costs nothing: it is replaced in place, and editing still keeps it.
- **A pinned tab stays at the start of its pane's bar**, after those pinned before it, via Pin Tab and Unpin Tab on its menu or in the palette. It keeps its title and close button: libadwaita's own pinned tabs shrink to their icon, and a note's name is what tells it apart. Its mark is `view-pin-symbolic`, tooltipped Pinned, and no button: unpinning is a menu choice, not a stray click. Outside this vault keeps the slot on a loose tab; a pinned preview is a kept tab.
- The boundary holds whatever moves a tab — a drag, `Ctrl+Shift+PageUp` / `PageDown`, a drop, Move Tab: a tab passes the pinned ones only by being pinned. Pinning or unpinning puts the tab on the boundary.
- Pinned is the window's, not the pane's: a pinned tab moved to another pane is pinned there too, at the end of its pinned tabs, and one dragged into another window is pinned there afresh. The session keeps the pins.

### Palette

- One `AdwDialog` with a `GtkSearchEntry` and a `GtkListView`; a leading `>` switches file mode to command mode (VS Code's convention).
- Go to File also lists each note a link names that is not written yet, after the files at the same score and marked Not created; picking one follows it as the link would, offering New File with its path typed in.
- It finds a note by its front matter's `aliases:` (or `alias:`), behind a file at the same score: the row reads the alias over the note's path, and picking it opens the note.
- A typed query also ranks the files this window opened that the index does not list (a gitignored build output, a file in an unwalked folder); one whose file has gone leaves the history.
- Its file list is kept warm in the background, not fetched when the dialog opens: a palette that is not instant is not a palette.
- Open Recent is the same dialog over the recent vaults, read as the start screen reads them, ending with Open Folder… and Open Remote…, so one surface reaches every way of changing vault and is never empty. The window's own vault is left out: picking it could only raise this window.

### Find bar

- One `GtkSearchBar` per pane, stacking the find/replace row and the go-to-line row. A pane owns its document, so the bar is the pane's: a row between the tab bar and the document, pushing the document down rather than covering it; a split searches two notes at once, each with its own query, mode and open state. `Ctrl+F` and its neighbours open the bar of the pane the reader is in.
- Over a shell the bar goes and the chord opens nothing: vte keeps its own scrollback.
- F5 over a note takes the pane tree off screen, so the presented pane lends its bar to the editor column for the duration.
- The readout says "3 of 12" in the buffer, a PDF and the rendered preview alike; past WebKit's 500-match ceiling the preview's total is a floor and no position is claimed.
- In a buffer the bar searches folded text too, as VS Code does: the count includes it, a step into a shut block or a collapsed comparison run opens it, and a replacement there leaves it shut.
- Previous and next are `go-up-symbolic` and `go-down-symbolic`: the matches are places in a vertical document, and back and forward arrows read as history.
- **Escape closes the bar wherever it is pressed**, from the bar ahead of `GtkSearchEntry`'s own Escape (which only clears the query), and from the document once nothing nearer the focus has answered it.

### Start screen

- Shown when launched without a vault path, and again from Open Folder…, Open Remote…, Open Recent… and Close Vault.
- With no recent vault it is an `AdwStatusPage`: the app icon and name over Open Folder… and Open Remote…. Otherwise the two buttons, a `GtkSeparator`, then the recent vaults under a search field, top-aligned so the field stays put while the list narrows and scrolls: the separator divides the ways to a vault the app has never seen from the ones it has.
- The list keeps every vault ever opened, newest first. A local folder found gone is dropped for good, here or in Open Recent; a remote one cannot be checked without dialling out, so it stays until removed by hand.
- The keyboard starts in the search, which takes what is typed anywhere on the screen and narrows by name and path, case aside; Enter opens the first row left, with nothing typed the newest vault.
- A row names its vault as the tree names a folder, the folder over its path; a remote one shows the address it was opened by (`me@host:/srv/vault`) under `network-server-symbolic`. A terminal session is a row too, its name over "Terminal session" under `utilities-terminal-symbolic`, until its state file is gone.
- There is only ever one start screen: a second Open Folder… presents it again. Opening a vault gives it a window of its own; Close Vault takes the current window away, releasing its vault, worker and WebKit process.
- **Open Remote…** asks for a host and a path in an `AdwAlertDialog`: the hosts from `~/.ssh/config` sit in a menu beside the host field, and the path field completes against the host and takes `~` for the login's home.
- That completion dials out before Connect is pressed, so it must never surprise: one attempt per dialog, made only once the keyboard is in the path field and a host is named; `BatchMode=yes`, so it never raises a prompt; a failure offers and says nothing; a five-second deadline, since a field that goes quiet is worse than one that never completes; and a control socket of its own, never an open vault's.
- `~` is resolved from the home the host reports, so no address carries a tilde into the config, the cache or a socket name; until the host has answered, the form says so and Connect stays off.
- The path field takes the keyboard as the name dialogs' does (Keyboard, Notes), Return pressing Connect where no row was arrowed to.
- **Open Folder…** in a remote window is this form, filled with the window's host and path and its folders asked for at once, which is how a vault opened at a mistyped path is steered. What it picks replaces the window, the vault it was on being the one corrected; the path it is already on only raises it. Open File… there stays this machine's chooser, not started at the vault's root, and opens its pick as a tab from outside the vault.
- The same form is **New Remote Terminal…**, with Open for its button, filled from the active remote tab's host and directory, else the window's remote vault; it opens a shell on that host in this window.

### Primary menu

- Four sections, by what an item changes: the files in this vault (New File, New Folder, Open File…); which vault the window is on (Open Folder…, Open Remote…, Open Recent…, then Close Vault); what the window shows (New Terminal, Presentation Mode); the application (Preferences, About accent, Quit).
- It holds what the window does whichever tab is in front: a command acting on the document (Save, Save As…, Find, Toggle Preview) keeps its chord and palette entry and has no item here.
- Labels come from the action table, so the menu and the palette cannot disagree.
- The sections are cut to what the window can do: without a vault, no New File, New Folder or Close Vault, which could only toast. A window of shells adds Save Session after Open Recent… (a focused shell keeps `Ctrl+S`) and, once named, Close Session after it. A remote vault's window has New Local Terminal beside New Remote Terminal….

### New Window

- **One vault never has two windows** (VS Code's rule): a vault with a window raises it, whoever asks, since two windows over one index, session and watcher have no shared state to keep in step. New Window therefore opens the start screen.
- It is both an `app.new-window` GAction and a `new-window` desktop action, since GNOME Shell looks for either before offering New Window in the launcher's menu; `accent --new-window` asks for it from a second process.

### Window without a vault

- A file named on the command line or handed over by the file manager, or `accent --terminal`, opens in a window with no vault: no index, no watcher, tabs keyed by absolute path, and no session, since a window opened on one file is opened the same way again.
- A window of shells saved under a name is the exception: keyed `terminal://<name>`, listed among the vaults, one window each, and written like a vault's (Save asks for the name once), so its shells come back when it is opened again. Close Session, after a question saying how many shells end, ends them, forgets the session and leaves for the start screen.
- Its sidebar holds the Outline pane alone, the others being views of an index, and starts hidden.
- There are two unnamed ones at most, one per kind, and the launch that builds one decides its kind for good: `accent --terminal` goes to the terminal window and a file from outside the open vaults to the documents window, so a PDF opened from Files never lands among the shells. What is opened in either by hand stays there without changing its kind, and a closed one is built again by the next launch of its kind.
- Closing an unnamed window ends its shells, nothing being able to take them up; an empty shells window says No Shell Open.
- Everything that needs the vault says so instead of failing quietly.

### Preferences

- `AdwPreferencesDialog` of `AdwPreferencesGroup` / `AdwSwitchRow`, an `AdwComboRow` each for the theme and the focus mode (its subtitle saying what the level fades), and a destructive `AdwButtonRow` for Restore Defaults.
- The document font's Reset is an icon button (`document-revert-symbolic`): the font button already shows the whole font name, and a text button would leave it nowhere to go.
- There is one config per process, so a change made anywhere — the dialog, the palette, the drawing ring, a rebound shortcut, Leave Out of Search — takes effect in every window at once, redoing only what it moved, and is written a second later, so a run of picks is one write.
- `config.toml` is watched: a hand edit is taken in as it lands and never written over. accent's own unsaved changes stay on top key by key; where both changed one key the file wins, and the log says so.
- A file that does not parse is neither taken in nor written over: a toast and a log line say so once, accent keeps its running settings, and what changes meanwhile is written once the file parses again, or lost if accent quits first. Only a file that fails to parse at startup is moved aside to `config.toml.broken`.

### Tooltips

- The full vault path with `$HOME` as `~`, on tree rows and on tabs.

### PDF

- A `GtkScrollable` widget painting cached tiles, sharp at any zoom, with a low-resolution stand-in under a page whose tiles have not arrived; one render thread per document, which also opens it, so a large file costs nothing at startup.
- Pages are recoloured onto the theme's paper and ink: light leaves them alone, dark and both Solarized halves remap them. A theme change renders every tile again, the colours being baked in.
- A drag selects the text under it, across page breaks as readily as within a page.
- A secondary click on the page: Copy and Copy Link to Selection while something is selected, then Add Page, Insert Page, Delete Page and Export Highlights — window actions in sections, the terminal's idiom, so the palette lists them and they can be rebound.
- **A highlight is a link** (Architecture): Copy Link to Selection puts `[[paper.pdf#page=1&selection=0,6,0,15|the quoted text]]` on the clipboard, one per page the drag covered, and pasting it into a note makes the highlight, painted in the accent at alpha 0.2 under the selection and the search marks. Clicking one opens the note holding it; following one from a note goes to the page and shows the selection. A link whose numbers no longer fit falls back to the text its alias quotes, then to the page.
- Export Highlights writes them into the file as real `/Highlight` annotations in the accent, skipping quads it already has.
- The drawing tools are a **ring**: Pen, Highlighter, Eraser, Line, Rectangle, Circle and Adjust orbiting a hub that floats over the page, opening in the top right (the page is read from the left, a hand comes in from the bottom right), dragged anywhere by the hub and kept where this window left it. The header's Drawing toggle opens and closes it.
- With a tool in hand the pointer is the plain arrow over the whole page, the I-beam inviting a selection that will not happen; under a stylus it is a small dot where the ink goes.
- Pen draws `/Ink` at a uniform width in the accent; Highlighter draws it wide and translucent with a `Multiply` blend, darkening the text rather than covering it.
- Eraser takes every whole stroke whose edge the pointer's path comes within 4 pt of, testing between reports as well as at them so a quick pass cannot step over a thin line; one drag is one Undo step. Partial takes only what lies within reach and leaves the rest as strokes of their own in the same colour, width and opacity, drawn straight between flattened points (a spline would bow a rectangle's edges); a crumb under 1 pt goes, and a stroke another editor drew as several paths or in its own space is left alone rather than redrawn wrong.
- Line, Rectangle and Circle are `/Ink` too, from press to release — a line snaps onto an axis within 7°, a circle grows from the press — because pdfium-render gives an appearance stream to nothing else.
- Adjust takes hold of any ink stroke, ours or another editor's: eight handles, the middle moving it, an edge stretching one axis, a corner scaling both alike; `Ctrl+Z` puts it back.
- A second orbit holds the tool's options: three widths, and for the pen, the shapes and the highlighter six swatches (the accent, four hues round the wheel by the CSV rule, and black); for the eraser, Whole Strokes (`edit-delete-symbolic`) and Partial (`edit-cut-symbolic`). Each pick is kept in the config's `[drawing]`.
- With a tool in hand the other tools dim to Adwaita's `--dim-opacity`, so the options read as that tool's; a dimmed tool comes back under the pointer or the keyboard and still takes a click. A press between the ring's buttons reaches the page.
- With a pen on the seat the mouse selects and only the pen draws, its eraser tip erasing under any tool; "Draw with the Mouse" in Preferences gives the mouse back. A finger never draws: touch moves the page. The pen is known by the event's device tool, since on Wayland a tablet's events come through a device whose source is a mouse.
- A stroke stays painted over the page until the re-rendered tile carries it.
- Ink and page edits are written into the file itself, atomically and gated on the etag the document was read at; the tab does not reload over its own write.
- Insert Sketch puts a blank A4 page beside the note, embeds it and hands it the pen.
- **The thumbnail strip organises the pages.** A thumbnail dragged along it moves its page, an accent bar marking the gap it lands in (none beside where it already is), the strip scrolling while the drag rests near an edge. The thumbnail under the pointer carries two round `.osd` buttons: Delete Page (absent on a one-page document, which a PDF keeps) and Insert Page Here on the gap below. A click goes to its page on release, a press being how a drag begins.
- The page's menu and the palette have Insert Page and Delete Page for the page being read, the palette Move Page Up and Move Page Down.
- A delete asks first in an `AdwAlertDialog`: nothing puts the page back, Undo walking ink alone.
- A move reorders the page tree, so bookmarks and links into the page follow it. After Insert the reader is on the new page; after a move or a delete, on the page they were reading, or the one that took its place.
- Notes name pages by number and are not rewritten, so an edit that leaves highlights pointing at other pages says how many in a toast.
- The stand-ins have their own budget, a quarter of the tiles', so scrolling a long document end to end does not keep every page.

### Diagram

- A draw.io file (`.drawio`, `.dio`, `.drawio.xml`, or an `.xml` whose first element says so) in a tab of its own, on a canvas painting `accent-drawio`'s display list: fitted to the window on open and at `Ctrl+0`, zoomed around the pointer and scrolled like a PDF, panned by middle-drag or Space+drag.
- The page is drawn in the document's own colours on a white sheet with a hairline edge; the theme only paints around it (Colour).
- Selection follows draw.io: the outermost group first, one level further in on each click after; nothing on a locked layer, which never receives a new shape either (the first unlocked layer does). Shift adds; a band over empty page takes what it wholly covers; a drag moves the selection on the page's grid (Alt for free); an arrow moved without its shapes lets go of them.
- A single shape carries eight accent handles, each edge resizing on its own, and a ring beyond its top-right corner turns it in draw.io's steps (15° near the ring, 5° a little out, whole degrees further, tenths with Alt), as the Properties pane's Rotation row does. A turned shape's handles turn with it, and it is resized in its own frame, the side not held staying put.
- The ring is the PDF's, with select, rectangle, ellipse, text, connector and image, and the tool's options on its outer orbit (square or rounded corners; a connector's route and arrow). It is out whenever a diagram is in front unless the Drawing toggle put it away; select is its resting tool and dims nothing, and a shape drawn or dropped puts the tool down again, a connector staying in hand.
- With the connector in hand the shape under the pointer shows its connection points as small accent crosses, the one in reach lit, and an end dropped on one is pinned there. Otherwise an end attaches to the shape under it only when its other end lies outside that shape: an arrow drawn within a slide's text box is on it, not leaving it.
- A label is edited as Markdown in the note editor, floated over the cell at the label's size with an accent outline: opened by a double click (the innermost shape under it, or a label's own text), `Return`, or typing over the one selected shape (a bare arrow's opens halfway along it), finished by a click elsewhere, Escape or `Ctrl+Return`.
- A formula label is typeset by WebKit and painted as a picture.
- Pages are the Outline pane's rows.
- Every change is one undo step, saved a second after the last through the etag-gated save. A change on disk under unsaved edits raises a banner whose Resolve… asks Reload or Overwrite: there is no comparison of two diagrams to offer.

### Comparison

- A side-by-side line diff, in a tab rather than a dialog: it is something to work beside, and git produces them by the dozen.
- One involving a file — the working tree against the index, a note changed under its buffer, a sync conflict copy — happens in the file's own tab: the editor is the editable side, a read-only companion styled the same sits beside it, and the diff is highlighting over both.
- Rows are kept level by blank space above lines, never filler text, and a line's number stays beside its text.
- Unchanged runs beyond three lines of context hide behind a "⋯ N unchanged lines" button: all of them on opening, the caret going to the first change, then all but the run holding the caret. A hidden line's end-of-line diagnostic message is left off (the gutter icon still says it is there), or a collapsed run's messages would stack on one row.
- Typing re-diffs on each keystroke and moves no row that did not change.
- A differing hunk carries Take / Keep Both buttons on the Theirs pane, so a merge is a click or a keystroke into the note itself. A sync conflict and a changed-on-disk note keep the Keep Theirs / Keep Mine bar under the panes; Keep Mine on a changed-on-disk note writes against the version it showed. The banner that asked drops its button while its comparison is on screen.
- A staged change or a commit compares two texts that are not files, in a transient tab of two read-only panes keyed by what it compares, so asking twice updates one tab. It opens as the pane's preview, a list of changed files otherwise stacking a tab per click, and is re-read when the Git pane refreshes.
- A selection in the working-tree comparison puts Stage Selected Lines on the view's menu, in a section of its own, and one in a staged comparison Unstage Selected Lines: exactly the changed rows the selection covers go in or out, a row with no line on the selection's side going with the line above it. Without a selection the entry is absent.
- Every comparison follows the document zoom, page margins included.

### Language servers

- One client for every text tab: accent's own index answers for markdown (Architecture), and a real server (clangd, rust-analyzer, pyright, taplo, …) starts on demand for code and stops when its last document closes, so nothing runs for a file nobody has open.
- **Completion** is `GtkSourceCompletion` with one provider: kind icon, label, the server's detail, and documentation in the details panel, resolved when a row is looked at. Accepting applies the server's own edit, snippet stops and imports included, as one undo step.
- In a note, `[[` offers notes and `![[` every file; both write a note by its stem and anything else by its whole name, or by its path where the name would resolve to another file first.
- `[[` also offers a note only linked to so far, after the real ones, by its path from the vault root and marked not created, so a second link reaches the same file once it is written; and a note by each front matter alias, written `[[Note|alias]]` because a link resolves by the file's name alone.
- `[[Note#` (or `[[#`, this note) offers the note's headings by their text, as Obsidian links them.
- A markdown link's destination, `[text](` or `![alt](`, offers every file by its percent-encoded path from the note's folder — how the index resolves it; the row reads the name, and what was typed matches anywhere in the vault. `[text](#` and `[text](Other.md#` offer headings by their GitHub slug; a link written with the heading text still resolves.
- `#` offers tags, at the start of a line only once a letter follows a single `#`: writing a heading must not open a list.
- **Hover** is the server's markdown as Pango markup, then every diagnostic at that position, all four severities: a hint shows nowhere else.
- **Signature help** is a popover over the caret's line, the active parameter in bold, taking no grab so the call goes on being typed.
- **Diagnostics** underline the text, mark the gutter and print errors and warnings at the ends of their lines, cut with an ellipsis to the room the text column leaves; the hover and the gutter icon carry the whole message.
- The status bar counts them where a note counts words, and pressing the count takes the underlines, marks and line-end messages out of the text and back, the count and hover staying. Per tab, and not kept across a restart: a way past something in the way, not a preference.
- On a note they are accent's own: a hint on a link the index cannot resolve, and a warning with the converter's reason over each formula the preview shows as source — a warning because pulldown-latex rejects some valid LaTeX.
- **Outline** and **References** are the sidebar panes; **folding** is the gutter chevrons.
- A file whose server is not installed says so once — a toast on Go to Definition, a sentence in the Outline pane — and everything else keeps working.
- Which server answers for which language is a table in the app, overridden per vault in `config.toml` (`[vaults."<root>".lsp.servers]`); no preference, the answer being whether it is installed.
- **Ghost text** (Architecture) is the rest of the line as the vault has written it, in prose, painted dim after the caret; `Tab` accepts and `Esc` dismisses. It has a preference, Ghost Text, on by default: unlike a server it costs seconds of CPU and a hundred megabytes whether anyone looks or not.
- The completion popup wins every contest with it: while the popup is up nothing is asked or painted, so `Tab` is the selected row when there is one and the suggestion when there is not.
- Nothing in the popup is selected until an arrow key or the pointer picks a row, and until then `Return` and `Tab` are the editor's, as in VS Code and Obsidian: a list carries on or an item indents, and the popup goes.
- **A move asks the servers already running**, never one started for it, what it breaks in the code (`workspace/willRenameFiles`), before anything moves, a server reading the disk to answer. The files it names follow the notes in the one Update question; a moved Rust or TypeScript file no running server answered for is said to be unchecked. rust-analyzer answers only for a rename within one folder.

### Context menus

- `GtkPopoverMenu` from a `gio::Menu`, parented to a plain widget (States), in sections: what creates or opens; what copies a name or a path or leaves the app; Move to Trash alone. Tree rows and tabs share the shape and the handlers.
- **New File and New Folder are on every tree menu**, aimed at the folder clicked, the folder of the file clicked, or the vault root. The blank area below the rows has its own menu: those two, Upload Files… on a remote vault, and Paste. New Drawing is left off a remote vault's menu and the palette's says why: a new PDF is made at a path on this machine.
- **Cut, Copy and Paste are a section of their own**, over a **hybrid clipboard**, because a vault's files are not always on this machine. A local vault writes the real clipboard in GNOME Files' own formats, so a file crosses between accent and Files either way. A remote vault's Copy remembers vault-relative paths in the window and its Paste copies or moves them on the host, so duplicating a folder sends no bytes over the link.
- A Cut pasted **is a move**, with its one Update Links? (Imports?) question for everything the Cut took, so notes cut together that link each other are rewritten together. The rows waiting on it are dimmed until the paste.
- A clipboard file this vault does not hold is carried in; a folder from outside is refused, a transfer carrying files only.
- **Paste never replaces** — a paste has nowhere to ask — so a taken name gets GNOME's own mark: `notes (copy).md`, then `notes (copy 2).md`, a folder taking it at the end. Paste is drawn only while the clipboard holds something.
- **Show Hidden Files ends every tree menu**, in a section of its own, as in GTK's file chooser: it is about the listing, not the row.
- **Show in Files is a local vault's**: on a remote one Download… takes its place, and the tab menu's or palette's Show in Files says why in a toast.
- **Splitting is the tab menu's alone**: a split opens the active tab beside itself, which is not done to a file in the tree. So is pinning: Pin Tab or Unpin Tab ends the Move Tab section, as in GNOME Web.
- **Rename and Move to Trash are on the tab menu too**, acting on the page right-clicked, and only where it holds a file of this vault; on a shell, a comparison or a loose file they are absent rather than refusing.
- **There is no Move to…**: a row moves by dragging, or by typing a path into Rename, the keyboard's move.
- **A tree menu pins the highlight** to its row until it closes: the popover taking the pointer is a leave to the list, which would otherwise light the open file instead.
- A popover parented by hand is unparented **from an idle**, never from `closed`: `closed` comes inside the item's own `clicked`, and unparenting there silently drops the item's action.

### Empty states

- `AdwStatusPage` (States): no vault; no note open (in a remote window, the stored tabs waiting for the host); no search results ("Nothing in this vault matches this search", a hit being possibly a source file); no backlinks or references; no language server for the open file; nothing to outline.
- `.compact` in the sidebar, where the full size dwarfs a 200–420 px column.

### Feedback

- `AdwToast` / `AdwBanner` / `AdwAlertDialog`; see States.

### Loading

- The status bar's progress text; never a modal, never a blocked window.
- A background query whose answer replaces a list gets a `GtkProgressBar` across the top of that list, faded rather than hidden so nothing shifts, and only after 160 ms: a bar that appears and goes in one breath reads as a flash. Between queries its place says what the answer holds — "12 results in 3 files", `+` on a number the cap may have cut — in `.caption` `.dim-label`.
- **A bar belongs to the surface it is about to fill**, which allows one more: a remote vault's first connection, across the top of the document column, since that wait makes the whole column usable and can take seconds. It measures the upload of the server binary and pulses through the rest; a local vault's window never shows it.
- Elsewhere a 16 px `AdwSpinner` beside the control that started the work, unless that would move the layout: a sync's spinner takes the Sync button's place at its size, so the branch row keeps its width. The status bar's branch, the same action, greys out meanwhile.

## Typography

<https://developer.gnome.org/hig/guidelines/typography.html> · <https://developer.gnome.org/hig/guidelines/writing-style.html>

- Prose is **Adwaita Mono at the GNOME document font's size** until the document font preference overrides it: a vault is prose with code fences, tables and wikilinks in it, none of which line up in a proportional face, so the family is ours and the size the system's. One function (`editor::font_css`) writes the display-wide rule and each zoomed tab's, so a zoomed note cannot come out in another face. libadwaita's `--document-font-family` / `--document-font-size` are the upgrade from the hand-built provider.
- Code uses GNOME's **monospace** font, through a class and provider of its own (`accent-code`): one display-wide selector cannot answer for both faces.
- Code sits in the same clamped, centred column as prose, with the same page gutters: a file is a document whatever its language, and a column changing width from tab to tab is what the eye notices.
- Code wraps as prose does: the far end of a long line off screen is worse than a folded line. The scroller's horizontal policy stays `Automatic`, so a line unwrapped with `Alt+Z` stays reachable.
- A code tab's colours come from the GtkSourceView style scheme (`Adwaita` / `Adwaita-dark`, or `solarized-light` / `solarized-dark` under Solarized), never from our tags.
- **The syntax scheme is the one place the single-accent rule gives way**, deliberately: one derived colour can say *link*, not *keyword*, *string*, *comment* and *number* at once, and the platform's scheme answers light, dark and the user's theme with no palette of ours to keep in step. A note is still styled from the accent and the foreground alone.
- GNOME's monospace font appears in a note only inside the `code`, `codeblock`, `math`, `html` and `frontmatter` tags.
- Heading scale, relative to the document font: h1 1.6, h2 1.4, h3 1.2, h4 1.1, h5 and h6 bold at 1.0. `strong` and list markers are weight 700, `em` italic.
- Line numbers are a preference in prose, off by default; a code tab always has them, code being read by line number and a compiler error naming one. They sit outside the 48 px page gutter, so turning them on shifts the page rather than crowding it. Every line is numbered, headings included: the number and the hanging `#` markers are a gutter apart and read as two margins.
- They rest at 0.6 opacity and come back to the scheme's gutter grey while the pointer is anywhere in the gutter.
- The gutter's background is the view's, not the scheme's: Solarized gives line numbers a shade of their own, which would put them in a stripe against the one flat background.
- An ATX heading's `#` markers hang in the left gutter so its text, wrapped lines included, lands on the body column, as in Apostrophe. A setext heading has none; h5 and h6 markers are wider than the gutter and clamp at the window edge.
- A wrapped line carries on under its own indent, not at the left margin: under the text behind a list, enumeration or quote marker (a task item under its `[`), and one indent level deeper than any other indented line, in every text tab. The hang stops at 32 columns: a tag's indent is pixels, not a function of its line.
- Markup is styled as it is typed in a note below 16 KB. Above that a full pass outlasts a frame and waits for the 150 ms debounce, the cost being the buffer's re-tagging, not the parse.
- Line length is set by width, not by counting characters: the clamp's maximum is a share of the editor's own width, floored at 480 px and scaled by the zoom, and it tightens from three quarters of that, so a window too narrow for the cap still gives the text everything it has. The width read is the scroller's own allocation: the adjustment's page size would feed the cap back into itself.
- **The column's width is the user's dial**, Column Width in Preferences: a percentage of the editor's allocation, not the window's, since the sidebar and the preview take their share. Comfortable prose is 60 to 72 characters, and the user asked for a column of at least half the viewport: the default 50 % is the old fixed 800 px cap on a maximised 1920 px screen. The 480 px floor keeps a small window readable at about 52 characters. Zoom scales the cap with the gutters, so a zoomed page keeps its proportions until the column fills the editor.
- The preview caps its column at `56ch`, about 71 characters in Adwaita Sans: `ch` is a zero's width, much wider than an average letter, so measure rather than assume.
- Header capitalisation for buttons, menu items, tab titles and tooltips; sentence capitalisation for messages and descriptions. An ellipsis (…) only when the label needs further input before acting. No OK / Yes / No: the affirmative button carries its verb, such as Save or Discard.

## Colour

<https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1.7/css-variables.html> · <https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1.7/style-classes.html>

- The only three colour sources in code are `AdwStyleManager`'s accent, `Widget::color()` (the resolved foreground) and `StyleManager::is_dark()`; the comparison's hues below are the one place that bends. A rendered PDF page cannot read a CSS variable, so `theme.rs` hands the renderer the paper and ink, and stays the only file that writes a colour down.
- GTK CSS uses `var(--accent-bg-color)`, `var(--view-bg-color)`, `var(--window-fg-color)` and friends: never `@named_colors` (libadwaita replaced them with variables), never a literal hex.
- Editor tag colours are all derived (`highlight::restyle`): `link`, `wikilink`, `tag` and `image` take the accent in its standalone form, the colour the platform writes its own links in; `marker`, `frontmatter` and `listmarker` take the foreground at alpha 0.4, `quote` and `taskdone` at 0.6, each held above 2.8:1 against the page; `code` and `codeblock` get a foreground background at 0.07. No other colour is set anywhere.
- Three things depart from the single accent, each where one colour cannot carry the information:
  - the style scheme a code tab is coloured by (Typography);
  - a palette of hues rotated from the accent, for a CSV's columns and the git history's lanes: a sixth of the wheel per column, keeping the accent's saturation and value, column 0 the accent itself, a seventh column repeating the first hue rather than inventing a colour — derived at runtime, so it follows the system accent;
  - a comparison's green and red, and a warning's amber: fixed hue weights mixed with the resolved foreground (`diff::tint`), because a diff has to read as green and red and libadwaita publishes its success and error colours only as CSS variables Rust cannot read. The editor gutter's change bars take the same two, green for an added line and a red wedge where lines went, and the accent for a rewritten one.
- The preview stylesheet derives everything from three values: foreground, background, accent. WebKitGTK cannot see GTK's CSS variables, so it gets the background as a literal from `theme.rs`. One rule is not derived: a `$$` block gets `margin: 1.2em 0`, a heading's standing room, since a `<math display="block">` box carries none of a prose line's half-leading and two formulas would otherwise sit closer than two paragraphs.
- A diagram's page is painted in the colours the draw.io file gives it, in light and dark alike, the theme reaching only the surround and the accent only the selection: a diagram's colours are its content. The Properties pane may put any colour into the document; none is a literal in code.
- **`theme.rs` is the only file allowed to write a hex literal**, and one anywhere else is a bug the pre-flight grep catches. It holds libadwaita's `--view-bg-color` pair (`#ffffff` / `#1d1d20`) and their foregrounds, the Solarized palette (Ethan Schoonover's, MIT), and the terminal's ANSI palettes: Ayu and Ayu Light (MIT, from `mbadolato/iTerm2-Color-Schemes`) under the Adwaita themes, Solarized's own under Solarized. Only the dark one has a contrast floor in the tests (3.0:1 on `#1d1d20`): Ayu Light and Solarized are low-contrast by design and ship as their authors made them.
- One flat background: the sidebar, both header bars, the tab bar and the document all paint `var(--view-bg-color)` (`accent-flat`), so the window reads as one surface rather than banded panels.
- The find bar paints the note's own background (`--view-bg-color`) in every theme through `accent-flat`, which has to reach `searchbar > revealer > box`: Adwaita paints that box in the header-bar colour, a band that shows once the chrome around it fades.
- The 1 px paned separator is the only division. Under the pointer it takes the accent at the same width, dragged it thickens to 3 px, and a double-click puts it back at its default position.
- Four themes, chosen in Preferences: System, Light and Dark are `AdwStyleManager` colour schemes and paint nothing of ours; Solarized redeclares libadwaita's `--*-bg-color` / `--*-fg-color` on `:root`, so every widget follows untouched and it still follows the system between its halves. It leaves the accent, shade and border variables alone, so the one-accent rule holds in all four.
- Light and dark are the same design by construction: outside `theme.rs` nothing is picked per theme, so there is no second palette to keep in step, and the same holds for the accent, which the user can change at any moment.

## Spacing

- The scale is 6, 12, 18, 24, 36 and nothing between: 6 inside a control group, 12 between related widgets, 18 between groups, 24 for dialog and page padding, 36 for empty-state breathing room — the long-standing GNOME convention, the current HIG having no spacing page.
- The window opens at 1100 × 760; the sidebar's floor is 200 and its width is kept in the session.
- The editor's margins are 48 left and right, 24 top and 96 bottom, with 2 px above and below lines, all scaled by the document zoom with the clamp, so a zoomed page keeps its proportions and its character count. They sit off the scale on purpose: page gutters inside the clamp, not layout spacing.
- A vertical `GtkSizeGroup` keeps the two header bars one height whatever the interface font, so the switcher and the tab bar line up; `.accent-lone-header` cancels the extra padding libadwaita gives a header alone in its toolbar view.
- Every icon button in a header, the pane switcher's included, is centred rather than stretched, so it reads square at 34 × 34.

## Chrome auto-hide

The point of the app: the chrome fades while the user types and comes back the moment attention leaves the text, in every view mode. How much fades is the Focus Mode preference, three levels in one combo row under Theme rather than a switch per surface, each level containing the one before.

- **None**: nothing fades.
- **Medium**, the default: the header bars, the tab bars, the status bar, the sidebar's panes and the minimap go to nothing, the tree being one pointer move away. The pane switcher rides in the sidebar header and goes with the top band, so the two columns stay in step.
- **High**: Medium, plus every pane but the one being written in recedes to 0.3 (a PDF or a shell like a note), the dividers go — every paned handle, and the undershoot line a scrolled window draws against a flat bar, which would frame the receding panes — and the text in the focused view fades away from the carets, as in Apostrophe's focus mode. The split preview stays crisp.
- The fade: a line keeps `α(d) = F + (1 − F)·exp(−d²/2σ²)` of itself, `d` its distance in buffer lines from what the carets and the selection cover, σ = 2 and F = 0.3, the receding panes' floor. Neither is a preference. The unit is the buffer line: a paragraph in prose, a line in code, and a blank line between two paragraphs counts as one.
- It hides on the first keystroke into the editor, and on any key that steps through the document the keyboard is in, moving through the text being attention on it: in an editor tab the caret keys (arrows, Home, End, Page Up, Page Down, alone or with Shift or Ctrl); in a PDF or the preview the same keys plus Space, and a PDF's `n` and `p`. Not with Alt or Super, which make a chord; not under a completion popup, whose arrows pick a row; not while presenting, which owns the chrome. The key is heard at the window, so one that a column of carets or a PDF uses up still counts.
- It returns on pointer motion anywhere in the window, a wheel or touchpad scroll (which need not move the pointer), Escape, a focus change, a view-mode change, and any keyboard focus move out of the editor. Hover must never be the only route back, or a keyboard-only user is stuck.
- Firing an action is not a route back: a chord is typing, so `Ctrl+S` and the scroll chords keep the mode. The palette and a menu still reveal it, through the focus change and the pointer motion they bring.
- Coming back takes away everything a level faded, whatever the level is by then.
- Never hidden: the editor, toasts, banners, dialogs, an open find bar. Suspended while a dialog, banner, popover or the palette is open, but not the find bar: its matches are what the reader is working through, so at High a line holding one keeps all of itself.
- The chrome fades by a CSS `opacity` transition on `.chrome-hidden` and `.chrome-away`, opacity only, so the layout never shifts and widgets keep their size and focus order. `AdwToolbarView:reveal-top-bars` is the upgrade if CSS ever fights the toolbar view.
- The line fade is a band of the view's own background painted over each visible line, under the carets and the ghost text, not a tag: `GtkTextTag` has no opacity, a `foreground-rgba` would flatten link and syntax colours into one grey, and re-tagging is the churn a re-style already pays. It covers the page gutters and the hanging markers, stops at the line-number gutter, ramps over the chrome's 250 ms and lives only while the chrome is hidden.
- With `gtk-enable-animations` false the classes toggle with no transition and the line fade jumps with them, so nothing fades and nothing becomes unreachable (<https://docs.gtk.org/gtk4/property.Settings.gtk-enable-animations.html>).

## Keyboard

Every user-facing action is a `GAction` with an accelerator and an entry in the palette's command mode: without an action it cannot be scripted, tested or found, and without a palette entry it does not exist. <https://developer.gnome.org/hig/guidelines/keyboard.html> · <https://developer.gnome.org/hig/reference/keyboard.html>

### Files

- Save `Ctrl+S`, Save As… `Ctrl+Shift+S`, New File `Ctrl+N`, New Folder `Ctrl+Shift+N`, Upload Files (unbound), Close Tab `Ctrl+W`, Open File `Ctrl+O`, Open Folder `Ctrl+Shift+O`, Open Remote (unbound), Open Recent `Ctrl+R`, New Window (unbound), Close Vault (unbound), Quit `Ctrl+Q`.
- Open Folder…, Open Remote…, Close Vault and Quit are `app.` actions, since each outlives the window that fired it; they still enter the palette's recently-run list.
- **Save As…** is Rename's dialog with Save as its verb, so it names a path in the vault: the tab's content is written there and the tab follows it, while the original keeps what was last written to it (a PDF's strokes are written first, then the file copied). A file already there asks Replace, saying so when a tab has it open, which then closes unsaved; a folder is refused; a new extension reopens the tab as what the file now is. The copy's relative links are written as they are. Over a loose tab, a window without a vault, an image or anything that is no file it does nothing.

### Palette and find

- Commands `Ctrl+P` (also `Ctrl+Shift+P`), Go to File `Ctrl+E`, Find `Ctrl+F`, Replace `Ctrl+H`, Replace in Files `Ctrl+Shift+H`, Search Ignored Files (unbound), Find Next / Previous `F3` / `Shift+F3`, Go to Line `Ctrl+G`.
- Find, Replace, Find Next / Previous and Go to Line act on the focused pane's own bar. Replace over a selection starts in the replacement box, the selection being the query already.
- `Up` / `Down` in a query or replacement box, the find bar's or the Search pane's, walk back through the ones used earlier in this run and forward again to what was being typed, as a shell does; each kind of box shares one list, not saved.
- Search Ignored Files is the Search pane's `All` button by another route.

### Editing

- Duplicate Line `Ctrl+D`, Delete Line `Ctrl+L`, Insert Line Below `Ctrl+Return`, Toggle Comment `Ctrl+K`, Toggle Word Wrap `Alt+Z`, Scroll Viewport `Ctrl+Up` / `Ctrl+Down`, Add Caret Above / Below `Shift+Alt+Up` / `Shift+Alt+Down`, Paste as Plain Text (unbound).
- Duplicate Line is VS Code's Copy Line Down: every line the selection touches, the caret and selection moving onto the copy. Insert Line Below copies the current line's indent.
- **A column of carets follows VS Code**: typing, the deletes and every caret motion (Page Up and Page Down included) act at every caret, and Shift with a motion extends each caret's own selection. Typing, Backspace, Delete and a paste take each selection; a plain Left or Right collapses it onto its start or end, and Up, Down, Home and End collapse it and move on.
- A paste goes to each caret, a line each when the clipboard holds one line per caret. Duplicate Line, Delete Line and Insert Line Below take every line a caret or its selection covers, once, a run of lines as a block. Cut and copy take each caret's selection joined by newlines top to bottom, or every caret's whole line where none has one.
- Overlapping selections, and a caret touching a selection, become one; two that only meet stay two. Undo and Redo put the carets and their selections back with the text. The other selections are painted in the primary's colour.
- A modifier on its own, AltGr included, and any chord the column has no use for leave it up. Escape ends it and keeps the primary's selection (a second Escape is GTK's), and so do a dead key or Compose, whose next keys are the input method's, and anything that moves the caret or edits at it alone — a click, `Ctrl+A`, `Ctrl+Home`, a completion. No completion is offered unasked while a column is up, and no ghost text; `Ctrl+Space` still asks.
- `Ctrl+X` and `Ctrl+C` with nothing selected take the caret's whole line with its newline, so the paste that follows opens a line instead of splicing into one. They are the widget's own chords refined, not bound here, so they stay on the never-bind list; the context menu's Cut and Copy do the same with nothing selected.
- Copy and cut put plain text on the clipboard, so a paste takes the styling of the text it lands in; a middle-click paste, GTK's own copy of the selection, is styled again once in.
- **A URL pasted over a selection in a note links it**: with one selection on one line that is not an address itself, and a clipboard holding a single `http`, `https` or `mailto` address, `Ctrl+V` writes `[selection](address)` as one undo step. Anything else, a column of carets and every code file included, is the ordinary paste. Paste as Plain Text is the way round it, unbound because `Ctrl+Shift+V` is Paste in Terminal's chord, which a text tab in front answers with it.

### PDF

- Copy `Ctrl+C`; next and previous page `Space` / `Shift+Space`, `n` / `p` and `Right` / `Left`; `Up` / `Down` scroll a step and are not paging.
- Next Page and Previous Page are palette commands with no accelerator, fired by those keys as the tab's own: each is a typing key, and an application accelerator is dispatched at the window ahead of whatever has the keyboard, so `Space` in the table would be taken from the editor, the terminal and every entry.
- Back and Forward are the window's (History): a PDF keeps no stack of its own. Only jumps go in — a link, an outline row, a page typed into Go to Line, a search's first step; paging is reading, and a history of single steps would have nothing to go back to.
- Ctrl held over a link previews where it leads without following it: a band of the target page around the destination, or an external link's address.
- Fit Width and Fit Height from the zoom readout's right-click and the palette; Invert PDF Colours from the palette.
- Drawing `Ctrl+Shift+I` opens the ring. Pen, Highlighter, Eraser, Line, Rectangle, Circle and Adjust (unbound) are on the ring and in the palette, a tool picked from the palette bringing the ring out.
- Copy Selection, Copy Link to Selection, Export Highlights, Insert Sketch, Add Page, Insert Page and Delete Page are in the palette and the page's menu, Move Page Up and Move Page Down in the palette, all unbound: the thumbnail strip is where pages are organised by pointer.
- While a pen is out `Ctrl+Z` takes back the last stroke, erase or move of this session and `Ctrl+Shift+Z` or `Ctrl+Y` makes it again, as in a note, and `Escape` puts the pen down: the tab's own keys, like Copy. Undo Drawing and Redo Drawing are also the header's two buttons and palette commands.
- Find and Go to Line keep their chords; over a PDF they search the document and go to a page.

### Diagram

- The canvas's own keys, as a PDF's are, firing actions the palette lists without an accelerator: Undo Diagram Edit `Ctrl+Z`, Redo `Ctrl+Shift+Z` / `Ctrl+Y`, Delete Selection `Delete` / `Backspace`, Select All Shapes `Ctrl+A`, Edit Label `Return` (or any character typed over one selected shape, which becomes the label's first), Next and Previous Diagram Page `Page Down` / `Page Up`. The arrows nudge the selection a unit and Shift+arrows ten; Space held pans; `Escape` puts the tool down, then clears the selection.
- Duplicate Selection is `Ctrl+D`, Duplicate Line's chord, which a diagram in front answers; `Ctrl+Return` finishes a label being edited; `F2` stays the file's Rename.
- The tools (Select and Move, Add Rectangle, Add Ellipse, Add Text, Add Connector, Add Image…), Bring to Front, Send to Back, and Add, Rename and Delete Diagram Page are unbound, on the ring and in the palette; a tool picked from the palette brings the ring out, and the same tool twice puts it down.

### History

- Back / Forward `Alt+Left` / `Alt+Right`, and the mouse's side buttons (8 and 9) through the same actions.
- **One history per pane**, of places rather than tabs: a document and a position in it, a caret or a PDF's page anchor. Back may switch tabs within the pane and never moves the keyboard to another pane, which owns its document as it owns its find bar.
- What goes in is a jump — a wikilink followed, a search hit opened, Go to Definition or References landing, a page or line typed into Go to Line, a find's first step (so Back returns to where the search began), a switch to another document in the pane — and an edit, coalesced: keystrokes within ten lines and ten seconds of the last entry replace it, so a typed paragraph leaves one mark.
- A hundred places. Closing a tab drops its places, nothing being left to go back into.

### Zoom

- Zoom In `Ctrl++` (also `Ctrl+=` and the keypad), Zoom Out `Ctrl+-`, Reset `Ctrl+0`, and `Ctrl+scroll` over a document, a PDF, an image, the preview or a terminal — never the chrome, which keeps the system interface font.
- The chords go to the active tab: a PDF fits its pages, an image is given a size of its own and centred, a terminal scales its own font (a grid of columns, not a page of prose), a document scales the display-wide one, and a status page and a diff have no zoom. The preview is zoomed with the editor, so presentation follows.
- A step is a tenth for the document font, a PDF page and an image alike, from the chord, the wheel or a pinch, and lands on the next tenth: a page fitted at 137 % steps to 140 %. A pinch follows the fingers in the same tenths, around the point between them. A step is taken from the zoom asked for, never read back off the rendered pixels, whose rounding would leave it stuck.
- The range is a tenth to eight times for a page or an image, a half to triple for the document font.
- The readout at the end of the status bar is the Reset control: clicking it is `Ctrl+0` — 100 % for a document, Fit Height for a PDF, though a PDF opens at Fit Width.
- Fit Height fills the viewport's height with one page, edge to edge, landing on the top of the page being read: any margin would only show the next page. A pane too narrow for that fits the width, so the whole page stays on screen.
- A PDF always shows its zoom (`Fit Width`, `Fit Height` or `N %`), fitting being a zoom too, and an image likewise (`Fit` or `N %`); a document and a terminal only off 100 %; a status page, a diff and a window with no tab never. Right-clicking it over a PDF offers Fit Width and Fit Height; the readout takes that press whatever the tab, so it never reaches the window handle under it.

### Terminal

- New Terminal `Ctrl+J`; `Ctrl+W` closes the tab and ends the shell, where closing the window does not. New Remote Terminal…, New Local Terminal, Save Session and Close Session are unbound.
- In a window of shells with no note in front, `Ctrl+S` saves the session; a focused shell keeps that chord, so Save Session is in the primary menu and the palette.
- Inside a shell, Copy in Terminal `Ctrl+Shift+C` and Paste in Terminal `Ctrl+Shift+V` are rows like any other: they rebind, list in the palette and name the shell's menu items. Over a text tab `Ctrl+Shift+V` is Paste as Plain Text. `Ctrl+PageUp` / `Ctrl+PageDown` stay `AdwTabView`'s.
- **A focused shell wins by default**: while a terminal has the keyboard the window keeps a small reserved set and unbinds the rest of this table, so `Ctrl+A`, `Ctrl+C`, `Ctrl+D`, `Ctrl+E`, `Ctrl+K`, `Ctrl+L`, `Ctrl+R`, `Ctrl+U` and the rest of readline behave as in any terminal. Reserved: `Ctrl+W`, `Ctrl+Tab`, `Ctrl+J`, the three zoom chords, `F11`, the Move Tab and Move Divider chords, and every chord spelled with both `Control` and `Shift`.
- The rule lives at the window, not on the shell: GTK dispatches application accelerators at the window ahead of the VTE, so unbinding them is the only way a key gets through.
- `Ctrl+W` is the budgeted cost — readline loses delete-word, which `Alt+Backspace` still does; the zoom chords are the same trade, a terminal having its own zoom; `F11` means nothing to readline or curses and is what GNOME Terminal keeps.
- A rebound chord follows the rule of the default it replaced.
- The table is the application's, one for every window, so the shell that narrows it is the one in the active window: moving to another window gives that window its chords back.

### Dismiss

- `Escape` is the way out of whatever is up, one thing per press.
- In presentation mode it ends presentation and does nothing else, ahead of everything, so a presented shell or preview cannot keep it.
- Otherwise it brings the hidden chrome back, takes no key from anyone, and the nearest thing over the document takes it: a signature popover; failing that, the focused pane's find or go-to-line bar, from the bar or the document (only that pane's, so a query in the other half of a split survives); failing that, the comparison the pane's tab hosts stops, as Stop Comparing does. A diff that is a tab of its own is closed like any tab.
- It has no `GAction` and no palette entry: it names no one thing.

### Panes

- Sidebar `F9`, Files / Search / Tags `Ctrl+Shift+E` / `Ctrl+Shift+F` / `Ctrl+Shift+T`, References `Ctrl+Shift+B`, Git `Ctrl+Shift+G`, Outline `Ctrl+Shift+L`, Properties `Ctrl+Shift+A` (a diagram's), Show Hidden Files (unbound, also in Preferences → Files).

### Git

- Sync (unbound): pull then push, from the pane's button, the status bar's branch or the palette — one action, syncing the repository the active document sits in.
- Merge Branch…, Abort Merge and Delete Branch… (unbound) act on the repository the pane shows, the one whose conflicts it lists. Merge Branch… is also the branch popover's last button and puts the pane on screen; Abort Merge is the merge banner's Abort and says so when there is no merge to abort; Delete Branch… is the popover's trash buttons by keyboard and puts the pane on screen.

### Notes

- Rename `F2`, Move to Trash `Delete` (tree only), New from Template… `Ctrl+Shift+D`, Insert Template… (unbound).
- A name with `/` in it is a path from the file's own folder, `..` included, so Rename is the move too and the keyboard needs no second dialog. It opens with a file's name selected up to its last dot, so typing keeps the extension, and a folder's selected whole.
- A template names where its notes go with an `accent-target:` front-matter line, so `accent-target: Daily/{{date:%Y-%m-%d}}.md` is the daily note, and asking twice in a day opens the one already made. The directive never reaches the note.
- Insert Template… puts any template at the caret of the open note, whose stem is its `{{title}}`: a meeting is typed into the day's note, not filed as one.
- Every `{{cursor}}` is a stop: the first takes the caret, Tab moves to the next and past the last to the template's end, Shift+Tab back, Escape leaves the rest. So a template asks for its fields in the editor, with `#` and `[[` completion, rather than in a dialog, and Templater's prompts and pickers are left out. Stops and a completion's placeholders are all Tab walks: GtkSourceView gets no snippet files, so Tab after a word expands nothing.
- The placeholders are strftime's — `{{date}}`, `{{time}}`, `{{date:%Y-%m-%d}}`, a signed day offset `{{date-1}}` / `{{date+3:%A}}`, `{{title}}`, `{{cursor}}`: one small mapping instead of a moment.js subset.
- **Rename and New File are one rule**: both take the name as typed, read `/` as a path from the folder they were opened on, create the folders it names as New Folder would, and show the vault-relative destination in a dim line under the entry, the only place `../Archive/note.md` says where it lands.
- A dot-named name is taken like any other (`.gitignore`, `.config/init.lua`), and while Show Hidden Files is off the toast says the file is hidden, so it does not seem to vanish; only what the vault never lists is refused: `.git`, `.trash` and the temporaries.
- **A typed path completes**: the folders under what the path names so far are listed under the entry as it is typed, and the folder button beside it shows the same list for an empty entry. Folders only: the last segment is a name being invented. The list is a revealer inside the form, pushing the buttons down: GTK4 has no replacement for `GtkEntryCompletion`, and in a dialog this small any popover lands on the buttons.
- **The keyboard never leaves the entry**: Down opens the list and steps into it, the arrows walk it, Tab takes the selected row or else the first, Escape puts the list away before closing the dialog, and Return takes a row that was arrowed to and otherwise confirms — nothing is selected until aimed at, so a finished path confirms on Return. The entry stretches with the dialog, whose width the list decides. Nothing is created until the dialog is confirmed.
- Renaming a note out of `.md` is the one rename that asks first: the file stays in the vault, searchable and still found by a link naming its stem, but it stops being a note — it opens as plain text, and its own links and tags are no longer read. Update Links cannot cover it, leaving those links nothing to rewrite.

### Code

- Go to Definition `Ctrl+Shift+Return` / `F12` / Ctrl+click, one question for every text tab: on a note it follows a wikilink, and an external link opens in the browser. A bare `http(s)://` or `mailto:` URL under the caret opens in the browser from any text tab, ahead of the language server (a URL is not a symbol), and Ctrl+hover underlines it as it does a link.
- Fold / Unfold `Ctrl+Shift+[` / `Ctrl+Shift+]`, Fold All / Unfold All from the palette. Fold takes the innermost block holding the caret; the gutter chevron beside a block's first line is the same by pointer.
- A shut block keeps to the lines it hid: text typed, pasted or inserted next to it stays in sight, and a line put between its header and what it hides opens it.
- Nothing folds until a language server says what the blocks are, so there is no chevron column before it answers. Both chords carry Control and Shift, so they stay bound while a shell has the keyboard.

### Ghost text

- `Tab` accepts the suggestion after the caret, `Ctrl+Right` takes its first word, `Esc` dismisses it, and typing what it shows shortens it rather than dismissing it.
- None of them carries a `GAction`: each means something else when there is no suggestion, which is most of the time, and still does (`Tab` indents, `Ctrl+Right` moves by word).

### Views

- Toggle Preview `Ctrl+M` (Editor and Split), Toggle Minimap (unbound), Presentation `F5`.
- Presentation hides the sidebar, the tab bars and both header bars and shows the note rendered. A tab that draws its own document — a PDF, an image, a diff, a terminal — is presented as it is, the document column staying and only the tab bars going; a PDF fits a whole page meanwhile and gets its zoom back afterwards. `Esc` leaves it.
- It does not resize the window: fullscreen stays `F11`'s, so the two compose.
- The bars go through `AdwToolbarView::set_reveal_top_bars(false)`, not the opacity fade, which would leave an empty band; the pointer-motion reveal is off meanwhile.
- The status bar alone comes back while the pointer rests on its strip at the bottom of the document column, over the document rather than pushing it up, so nothing reflows under the pointer; it goes when the pointer leaves the strip or the window.
- It is not saved in the session: a window restored without chrome would be hard to leave.

### Window

- Fullscreen `F11`, Preferences `Ctrl+comma`, Primary Menu `F10`, About accent (unbound).

### Tab menu

- Copy Name, Copy Relative Path, Copy Absolute Path, Show in Files, Reveal in Sidebar, Pin Tab and Unpin Tab, all unbound: the tab menu's items, acting on the tab it was opened on, or from the palette on the active tab.

### Tabs and panes

- Next / Previous Tab `Ctrl+Tab` / `Ctrl+Shift+Tab`, `Ctrl+PageUp` / `Ctrl+PageDown`, `Alt+1` to `Alt+9`; Split Right `Ctrl+\`, the other three sides from the menus and the palette; Move Tab Left / Right `Shift+Alt+Left` / `Shift+Alt+Right`, Up and Down from the menus and the palette; Move Divider Left / Right / Up / Down `Ctrl+Alt+Left` / `Right` / `Up` / `Down`.
- The first pair walk the pane's tabs in most-recently-used order (Tabs); the rest stay `AdwTabView`'s, stepping along the bar and picking by number being what it means by them.
- `AdwTabView` gives up the two Tab chords and both Home / End pairs, whose capture-phase bindings took GtkSourceView built-ins from the never-bind list. The chords it keeps — `Ctrl+PageUp` / `PageDown`, `Ctrl+Shift+PageUp` / `PageDown`, `Alt+0` to `Alt+9` — are listed in `panes::CHORDS`, so the rebind dialog refuses them by name rather than let a command shadow the bar.
- Next and Previous Tab and the four Move Tab actions are in a shell's reserved set, as Close Tab is: a tab chord must mean the same over every tab.
- Move Tab moves the tab into the pane that way and splits one off where there is none, so the chord works in a single-pane window; the tab takes the keyboard with it. `Ctrl+\` and its siblings stay the always-split. Plainer arrows were taken: `Alt+Left` / `Right` are Back and Forward, `Shift+Alt+Up` / `Down` the carets, `Alt+Up` / `Down` on the never-bind list.
- **Move Divider** moves the divider of the nearest split around the active pane that runs that way — left and right a side-by-side split, up and down a stacked one — and does nothing where there is none. A step is a twentieth of the split on a grid anchored at the centre, where a split starts and a double-click puts it back, so the centre is always one press away; it stops at a pane's minimum width. Reserved in a shell with Move Tab: arranging panes has to work from any of them.
- **Known clash**: GNOME binds the four `Ctrl+Alt`+arrow chords to switching workspaces (`switch-to-workspace-*`), and the shell takes them first; where those defaults stay, the four rebind from the palette.

### Never bind

`Super`+anything (the shell owns it), `Alt+Tab`, `Alt+F4`, `Alt+F7`, `Alt+F8`, `Ctrl+Alt+*` (workspace switching) except the four arrows taken for the dividers (Tabs and panes), `F1` (help), `Ctrl+Shift+U` (IBus unicode entry), `Ctrl+Space` (input-method switch), and the GtkSourceView built-ins (`Ctrl+Z`/`Ctrl+Y`, `Ctrl+A`, `Ctrl+X`/`C`/`V`, `Alt+Up`/`Alt+Down`, `Ctrl+Home`/`Ctrl+End`, `Ctrl+Shift+Home`/`Ctrl+Shift+End`).

### Rebinding

- The table is the set of defaults. The accelerator beside a command in the palette is a button: clicking it asks for a new chord, Backspace unbinds the command, Restore Default puts the default back.
- Overrides live in `[shortcuts]` in `config.toml`, keyed by full action name (`"win.save" = ["<Control>s"]`), only what the user changed being written, so the table keeps deciding the rest. An empty list is deliberately unbound, and an unbound action still lists in the palette, so nothing becomes unreachable.
- A chord in use is refused by naming the command that holds it; a clash from a hand edit is marked on both rows.
- The never-bind list is shown in the rebind dialog, not enforced: the desktop and the widget keep those chords whatever is stored.

### Widget chords

- Five Editing chords are the widget's class shortcuts — GtkTextView's `Ctrl+Up`/`Ctrl+Down` (paragraph movement) and `Ctrl+K` (delete to line end), GtkSourceView's `Shift+Alt+Up`/`Shift+Alt+Down` (`move-viewport`) — which run before the window's accelerators, so a capture-phase controller on the window claims them for our actions, except in a shell, where `Ctrl+K` kills to the end of the line. Taking a widget binding needs that deliberation; it is not the default way to add a shortcut.
- The other way round, a controller on one of our own widgets never sees a chord the table holds, so a chord a focused widget must own is handed to it by the action: that is how the commit box keeps `Ctrl+Return`.

### Deviations

- The HIG reserves `Ctrl+P` for Print and `Ctrl+Shift+P` for Print Preview; accent has no printing, and its users come from VS Code and Obsidian, where both open the palette. Revisit if printing is added.
- There is no shortcuts window: `AdwShortcutsDialog` needs libadwaita 1.8, we build against 1.7, and `GtkShortcutsWindow` is deprecated. Until the floor moves, the palette's command mode is the shortcuts reference, so it shows the accelerator beside every command.

## Iconography

<https://developer.gnome.org/hig/guidelines/ui-icons.html>

- Symbolic icons from the Adwaita theme, with the shipped sets below as the exception: no other bundled glyphs, no emoji.
- Every theme name the code asks for (`grep -rhoE '"[a-z0-9-]+-symbolic"' apps/gtk/src`) must exist in `/usr/share/icons/Adwaita/symbolic/`.
- Where the theme has no glyph, the nearest name that says what the control does: Tags is `user-bookmarks-symbolic` (there is no `tag-symbolic`); the Git pane and Sync are `mail-send-receive-symbolic`, arrows leaving and arriving, its one network action (there is no git glyph); References is `mail-reply-sender-symbolic`, an arrow turning back, for "what points here" (`insert-link-symbolic` is a text-insertion mark, drawn off-centre); Outline is `view-list-bullet-symbolic`; a tab from outside the vault is `document-open-symbolic`, the action that put it there.
- Diagnostics are `dialog-error-symbolic` and `dialog-warning-symbolic` in the gutter, the pair every desktop reads as those severities. A fold's chevron is `go-down-symbolic` open and `go-next-symbolic` shut, as the sidebar's disclosures are.
- Where nothing fits, ship a glyph in the app `GResource` under the `io.github.stroblme.Accent` prefix, drawn on the 16 px symbolic grid with `fill="currentColor"` so it recolours with the theme. Never a coloured icon. A GResource rather than hicolor, so a run from the source tree has them too.
- The shipped sets are [tabler-icons](https://github.com/tabler/tabler-icons) outlines (MIT, `apps/gtk/data/icons/LICENSE.tabler`) with their strokes turned into fills, GTK recolouring a symbolic icon by its `fill`: eighteen completion kinds (`lsp-<kind>-symbolic`), Adwaita having no glyph for a function or a type parameter; the drawing tools (`tool-pen`, `tool-highlighter`, `tool-eraser`, `tool-line`, `tool-rect`, `tool-circle`, `tool-adjust`, and a diagram's `tool-select`), the theme having no pen, marker or eraser, beside Adwaita's `insert-text`, `insert-image` and the Drawing toggle's `document-edit-symbolic`; and the file types.
- **Every file list leads its rows with a file-type icon** — the Files tree and its vault row, the palette's file rows, the Git pane's folders and files, search results, a tag's files, References — and the tabs carry none. The kind comes from the name (note, PDF, diagram, table, image, code, config, text, anything else) and is drawn by `filetype-<kind>-symbolic`: tabler's `markdown`, `file-type-pdf`, `sitemap`, `file-spreadsheet`, `photo`, `file-code`, `file-settings` and `file-text`, with `filetype-folder-symbolic` and `filetype-file-symbolic`. The PDF one letters `PDF` into the page, kept over `file-description`, which would make a PDF and a text file look alike.
- The prefix is `filetype-`, not `file-`: a GResource is searched as part of hicolor, after the user's theme, and themes ship `file-*` names of their own (WhiteSur's `file-link-symbolic`).
- **The name is ours, the artwork is not**: a theme may redraw any Adwaita name, and what it ships may not render (WhiteSur's `pan-*` files use a single-quoted `fill` GTK's recolouring cannot parse, so they draw nothing, silently). So prefer a name that says what the control does over one that says which way a panel opens — `edit-find-replace-symbolic` for the Replace toggle, `go-down-symbolic` / `go-next-symbolic` for the Git pane's disclosures — a generic shape being the likeliest to be redrawn. The `pan-*` names libadwaita's own stylesheet draws for a tree expander and a dropdown's arrow are the theme's problem, not ours.

## States

- Empty: an `AdwStatusPage` with a symbolic icon, a header-capitalised title, one sentence of body and at most one button (<https://developer.gnome.org/hig/patterns/feedback/placeholders.html>); in the sidebar `.compact`, or the icon alone takes 128 px of a 200 px column.
- A context menu never scrolls: parent it to a plain box and translate the pointer into its coordinates. GTK re-presents a popover only when its parent is allocated, which a widget with its own `size_allocate`, such as `GtkListView`, never is, so a menu hung off one freezes at its first size and scrolls.
- Loading is the status bar's progress text ("Indexing… 1200/42700 files"): indexing and saving never block the window, so no spinner covers content and no progress bar owns the window. Two bars are drawn, and no third without a line here: the sidebar's search progress, held back for its first two pulses so that a requery nobody asked for is over before there is anything to see; and a remote vault's first connection (Layout map, Loading). GTK4 has no indeterminate mode, so both pulse on timers of ours, removed when the work ends and when their window closes: a leaked `glib::timeout` keeps firing.
- **Stopping a first index is a pause.** The status bar's Stop keeps everything indexed so far, usable and with its links resolved, and reads "Indexing paused" beside Resume until Resume or the next open of the vault finishes it; nothing else walks the vault meanwhile, a local or a remote one. A stop during the scan writes nothing, since a partial scan would read every file it had not reached as deleted. The pause is not saved: the index is a diff of the disk, so opening the vault again is the resume.
- A toast for a thing that happened and is over ("Saved", "Moved to Trash"); a banner for a state that persists and needs a decision ("This file changed on disk", "A sync conflict copy of this file exists"); an `AdwAlertDialog` only for a choice that can lose data: overwrite, discard, delete permanently. <https://developer.gnome.org/hig/patterns/feedback/toasts.html> · <https://developer.gnome.org/hig/patterns/feedback/banners.html> · <https://developer.gnome.org/hig/patterns/feedback/dialogs.html>
- A sync conflict copy raises the banner on the tab showing its file, any text file, and never a toast per copy: on a synced vault that is a wall of them at startup. Conflicts on files nobody has open are counted at the end of the "Indexed N files" toast.
- The vault worker's own failures (indexing, link resolution, watching) are said once per vault and logged every time: nobody can act on them, and they recur as long as their cause does.
- Replace All in the sidebar rewrites files nobody has open, so past a single match it asks first; the one match already on screen, struck through, goes through on the click.
- A remote vault that stops answering is a banner across the window, every tab being affected and the state lasting. A link that was up comes back on its own: the banner counts down to each attempt ("Lost the connection to host · Reconnecting in 8 s") after 1, 2, 4, 8 and 16 s, then every 30 s, each with `BatchMode=yes`, so it never raises a passphrase dialog over the reader's work. Its button, Reconnect Now, is the one attempt that may ask.
- A first connection that failed and an outage past ten minutes keep a plain Reconnect under the last reason: neither comes back by itself. So does a refusal from the host — `serve` refusing a root that is not a folder ("cannot open the vault on host: /srv/x is not a folder") — which is not a dropped link and stops the countdown.
- While a remote window's stored tabs wait for the host, the document column says how many ("Waiting for host", "3 tabs will open when it answers.") and the tree that its files will show when it answers, rather than No Note Open and Empty Vault; a window closed before its host answered leaves the stored session as it found it. A window without a vault has no banner: its remote tabs say for themselves when the link went, and come back on a key press.
- A file that will not open as text — binary (a NUL byte, the test `grep` and `git` use) or over the 16 MiB cap — gets an `AdwStatusPage` in its own tab, not a dialog or a toast: the file is what failed, so the failure stays with it. Anything waiting for it to become a tab (a comparison, a jump to a match) is told in a toast ("Cannot compare notes.png: binary file"), the status page answering only the open.
- A file that is text but not valid UTF-8 opens read-only behind a banner with no button: the screen shows a lossy reading of the bytes, so the only safe answer is to leave the file alone.
- A tab has one banner and may have several things to say, so they queue and the one that can cost most shows: "deleted on disk", then "changed on disk", then a sync conflict copy (which blocks nothing), then the read-only report (which asks nothing). Taking one down brings the next up. Queued, not merged: `AdwBanner` has one button, and two questions share no honest label.
- **Saving never answers a question** (VS Code's rule). A buffer whose file moved under it holds the only copy of its edits and of the answer not yet given, so nothing writes until it is given; `editor::may_save` is the one predicate every save path asks. Autosave does nothing meanwhile, the banner and the tab's dot being the only signal, and writes the buffer nowhere else: a copy nothing reads back would be a second source of truth. `Ctrl+S` refuses too, through the overwrite dialog (over a note deleted on disk it writes it back, as that banner's button does). A closing tab or window still tries the write and asks when refused: a buffer on its way out has nowhere else to be kept. The etag gate has the last word, and a failed `stat` is never read as a change.
- **Closing while git works**: a command rewriting the working tree or the index — a pull (every Sync's first half), a merge, a switch, a commit, Stage, Unstage, Discard, Abort Merge — cannot be cut off without leaving the repository half-updated, so closing then asks "Git Is Updating Files", with Cancel and Close When Finished (suggested) and no answer that stops git. Waiting says "Closing when git has finished…" and closes once git lands; a failure keeps the window open under it. The question comes before any buffer is flushed, so the flush and Unsaved Changes run on what git left.
- A fetch and a Sync's push half are stopped instead and the window closes at once: a fetch moves only remote-tracking refs, and a remote takes a push whole or not at all, so what a stopped push did not send is the ahead count the next fetch shows. On a remote vault only the host knows which half a Sync is in, so a Sync there asks throughout, and a fetch is left to the host, which ends it within its bound once the link closes.

## Motion

- libadwaita's defaults only: no custom easing, no staggered reveals, nothing animating on load. The only timings we own are debounces, which keep the main loop free.
- The document follows the caret and nothing else: GTK keeps the insert mark on screen by itself (arrows, typing, find, Go to Line, a search hit, every extra caret), a wheel scroll away from the caret stays put, and a dialog opening and closing over it puts nothing back.

| Timer | Value | Where |
|---|---|---|
| Re-highlight after a keystroke | 150 ms | `editor/mod.rs::DEBOUNCE` |
| End-of-line messages cut again after a width change | 150 ms | `editor/mod.rs::DEBOUNCE` |
| Palette query | 50 ms | `palette.rs::DEBOUNCE` |
| Search progress pulse | 80 ms | `sidebar/search.rs::PULSE` |
| Connection progress pulse | 80 ms | `connect.rs::PULSE` |
| Automatic reconnect | after 1, 2, 4, 8, 16 s, then every 30 s, for 10 min; the banner's count ticks every 1 s | `reconnect.rs::backoff`, `GIVE_UP` |
| Preview re-render, a note's word count | 300 ms | `main.rs::RENDER` |
| Symbols and folds after an edit | 300 ms | `lang.rs::REFRESH` |
| References after a caret move | 300 ms | `references.rs::REFERENCES` |
| Outline row after a caret move | 100 ms | `editor/mod.rs::CURSOR` |
| Autosave | 1 s idle | `editor/mod.rs::AUTOSAVE` |

## Material 3 mapping

<https://m3.material.io/> · <https://developer.android.com/develop/ui/compose/designsystems/material3>. MOBILE_DESIGN.md says what to build; this says what each desktop surface became.

| libadwaita | Compose Material 3 |
|---|---|
| `AdwApplicationWindow` + `AdwToolbarView` | `Scaffold`, one screen holding the window insets |
| `AdwHeaderBar` | the `DocumentBar` row lying over the document |
| Sidebar panes and the palette | the Browse panel: Search, Files and Command chips over a `HorizontalPager` |
| `sourceview5::View` in `AdwClampScrollable` | `BasicTextField(TextFieldState)` styled from the same core spans |
| WebKitGTK preview | `WebView` over the same `markdown::to_html` output |
| `AdwTabView` + `AdwTabBar` | none: one document at a time |
| `AdwStatusPage` | a centred `Column`: icon, `headlineSmall`, `bodyMedium`, one `FilledTonalButton` |
| `AdwToast` / `AdwBanner` / `AdwAlertDialog` | `Snackbar` / an inline row above the content / `AlertDialog` |
| `AdwPreferencesDialog` | none: the app has no settings screen |
| `AdwStyleManager`'s accent | `primary` of `dynamicLightColorScheme` / `dynamicDarkColorScheme`, the page and ink staying the desktop's (<https://m3.material.io/styles/color/dynamic-color/overview>) |
| Chrome auto-hide | one `Chrome` state: the bar and buttons leave on scroll and return on scroll back or a tap, through `AnimatedVisibility` |

## Pre-flight checklist

Ten mechanical checks before shipping a UI change; none needs judgement.

1. `cargo fmt --all --check`
2. `cargo clippy -p accent --all-targets --locked -- -D warnings`
3. `cargo test --locked && cargo test -p accent --locked`
4. Headless smoke run: `make smoke` (its `XVFB_ENV` carries `GDK_BACKEND=x11` and `GTK_A11Y=none` as well as `G_DEBUG=fatal-criticals`; the comment above it in the Makefile says why each is load-bearing)
5. Dark: `gsettings set org.gnome.desktop.interface color-scheme prefer-dark`, look, then set it back to `default`
6. Accent: `gsettings set org.gnome.desktop.interface accent-color teal`, look, then set it back to `blue`
7. No stray colours: `grep -rnE '#[0-9a-fA-F]{3,8}' apps/gtk/src | grep -v theme.rs` returns nothing
8. Reduced motion: `gsettings set org.gnome.desktop.interface enable-animations false`, check nothing became unreachable, then set it back to `true`
9. Keyboard-only pass with the pointer unplugged: reach every action in the accelerator table, and get the auto-hidden chrome back without a mouse
10. `GDK_SCALE=2 target/release/accent testvault` for scaling, plus an IME check (ibus, type CJK into a note and confirm the preedit lands in the right place)
