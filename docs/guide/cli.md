# Command line and MCP

Use `accent /path/to/vault` to open a [vault](vaults.md), or add a note path to open it too.
`accent --terminal /path/to/folder` opens a [terminal](terminals.md) there.
For a [remote vault](remote.md), use an address such as `accent ssh://user@host/path/to/vault`.
Run `accent --help` for the other launch forms.

`accent-cli` can query a vault without opening a window.
For example, `accent-cli search --vault /home/me/Notes "meeting notes"` prints matching lines.
Use `accent-cli backlinks --vault /home/me/Notes Project.md` to find notes linking to `Project.md`.
The `tags` and `stats` commands show a vault's tags and index summary.

## Connect an agent

Set your agent's MCP server command to `accent-cli` with these arguments:

```text
mcp --vault /home/me/Notes --read-only
```

This starts a local stdio server, and it works while the Accent window is closed.
Read-only mode lets the agent search and inspect files without editing them.
Omit `--read-only` if you want to allow its note-writing tools.
For a remote vault, run the MCP server on the host beside the files.

The MCP tools include search, backlinks, tags, code exploration, and diagram rendering.
Diagram rendering also needs the Accent app and a graphical display where the server runs.
