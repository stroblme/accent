# Command line and MCP

Use `accent /path/to/vault` to open a [vault](vaults.md), or add a note path to open it too.
`accent --terminal /path/to/folder` opens a [terminal](terminals.md) there.
For a [remote vault](remote.md), use an address such as `accent ssh://user@host/path/to/vault`.
Run `accent --help` for the other launch forms.

`accent-cli` can query a vault without opening a window.
For example, `accent-cli search --vault /home/me/Notes "meeting notes"` prints matching lines.
Use `accent-cli backlinks --vault /home/me/Notes Project.md` to find notes linking to `Project.md`.
The `tags` and `stats` commands show a vault's tags and index summary.
CLI search accepts the same [qualifiers](search.md#narrow-a-search) as the Search pane.
Quote the query so the shell passes tags and date comparisons unchanged.

```sh
accent-cli search --vault /home/me/Notes '#research kind:pdf,note modified:7d'
```

PDF matches name their page, for example `paper.pdf#page=12`.
PDF text is read in the background while the app or an MCP server keeps the vault open.
A one-shot CLI query uses text already indexed.

## Connect an agent

Set your agent's MCP server command to `accent-cli` with these arguments:

```text
mcp --vault /home/me/Notes --read-only
```

This starts a local stdio server, and it works while the Accent window is closed.
Read-only mode lets the agent search and inspect files without editing them.
Omit `--read-only` if you want to allow its note-writing tools.
For a remote vault, run the MCP server on the host beside the files.

To start an agent connected to the open vault, add it under **Preferences → Commands** and run it from the vault's window.
For Claude Code:

```sh
claude --mcp-config '{"mcpServers":{"accent":{"command":"accent-cli","args":["mcp","--vault","."]}}}'
```

For Codex:

```sh
codex -c 'mcp_servers.accent.command="accent-cli"' -c "mcp_servers.accent.args=[\"mcp\",\"--vault\",\"$PWD\"]"
```

The MCP tools include search, backlinks, tags, code exploration, and diagram rendering.
Diagram rendering also needs the Accent app and a graphical display where the server runs.

## Explore and read files

Ask `explore` about names, paths, words, `[[links]]` or `#tags` to get ranked source excerpts with line numbers, outlines, links and call paths.
Its default code relationships match names within the same language.
It quotes call sites and lists implementations or subtypes where the scanner finds them.
Set `precise` to request a language server's call hierarchy and a trait's implementations when available.

`read_note` reads Markdown and Org sections, and returns a text file's etag for subsequent writes.
For a PDF, it returns indexed text under page headings without a write etag.
A Jupyter notebook returns cell sources by default, while `view: "raw"` returns its editable JSON and etag.
`write_note` needs the returned etag to replace an existing file.

## Compared with CodeGraph

`explore` answers the same kind of question as [CodeGraph](https://github.com/colbymchenry/codegraph)'s `codegraph_explore`, with notes, PDFs, links and tags in the same index.
We asked both seven questions about this repository, among them a function, a trait, a file path and a Kotlin method.
The numbers below come from accent 3.0.2 and CodeGraph 1.6.0 on a Xeon E5-2683 v4 server with 20 virtual cores in November 2026.

| | accent | CodeGraph |
|---|---|---|
| Index build | 3.0 s | 11.0 s |
| Index size | 47 MB | 95 MB |
| Median answer | 3,800 tokens | 6,200 tokens |
| Median answer time | 90 ms | 300 ms |
| Answers that need no second call | 6 of 7 | 3 of 7 |

An answer needs no second call when it quotes both the declaration and the use the question asks about.
Both tools fill a fixed budget, so answer size mostly reflects their defaults of 16,000 and about 25,000 characters, counted here at four characters per token.
Matching by name, accent counted four callers of `Edges::callers` where rust-analyzer and CodeGraph found two, and `precise` closes that gap.
These questions come from the codebase accent's scanner is tested on, while CodeGraph's tree-sitter parsers cover more than 30 languages against the scanner's nine.
`build-aux/explore-bench.py` repeats the measurement on a checkout of this repository.

## Jobs and shared terminals

Add `--allow-jobs` to enable detached command jobs and terminal tools on the machine running the server.
Jobs continue after the client disconnects.
They start an argument list directly, so shell syntax needs an explicit shell such as `sh -lc`.
The tools can start, list, read and stop jobs, and send literal input to terminals.
In read-only mode, only listing and reading are available.

To make a desktop terminal available, right-click it and choose **Share with MCP**.
Use **Revoke MCP Access** from the same menu to remove that access.
Sharing controls Accent's terminal tools, while clients allowed to run arbitrary commands retain the host user's normal access.
An unattached terminal's result includes a host-local attach command for taking it up interactively.
Finished jobs remain for seven days, with a rolling 16 MiB output tail read by byte offset.
