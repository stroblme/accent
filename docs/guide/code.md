# Code editing

Open a source file as you would a [note](notes.md).
Accent picks syntax highlighting from the file type and indents new lines as you type.
It wraps long lines by default, but `Alt+Z` toggles wrapping for the current tab.

## Navigate code

Open the Outline pane with `Ctrl+Shift+W` to jump to a declaration.
The outline can use Accent's index while a language server is starting or unavailable.
When a server reports foldable blocks, click a gutter chevron or use `Ctrl+Shift+[` and `Ctrl+Shift+]`.

Use `Shift+Alt+Up` or `Shift+Alt+Down` to add carets on neighbouring lines.
`Alt+J` adds a caret at the next match, and `Ctrl+Shift+L` selects all matches.
For a bird's-eye view, enable Minimap in [Preferences](preferences.md) or run **Toggle Minimap**.

## Language servers

Install a language server on the computer that holds your [vault](vaults.md).
Accent starts a supported server when you open a file.
For example, it tries `rust-analyzer` for Rust, `pyright` or `pylsp` for Python, and `texlab` for LaTeX.
On a [remote vault](remote.md), install the server on the host.

Completion appears as you type, or you can request it with `Ctrl+Space`.
Choose a suggestion with the arrow keys, then press `Tab` or `Return`.
Hover over a name for details, or use `F12`, `Ctrl+Shift+Return`, or `Ctrl`+click to go to its definition.
The Info pane shows references, while the status bar's diagnostic count toggles inline messages.

To choose a different server, set its command in `~/.config/accent/config.toml`:

```toml
[lsp.servers]
python3 = ["pylsp"]
```

You can also set a server for one vault under `[vaults."/path/to/vault".lsp.servers]`.
Without a server, you can still edit, search, and use the indexed outline.

## LaTeX and SyncTeX

Accent edits `.tex` files, and `texlab` adds language features if it is installed.
Build the document with SyncTeX enabled, for example `latexmk -pdf -synctex=1 paper.tex`.
Then use **Show in PDF** from the `.tex` tab's menu to find the line under the caret in the built [PDF](pdf.md).
To go the other way, `Ctrl`+click the PDF page or choose **Go to Source** from its menu.

Both commands need a matching local build with `.synctex` or `.synctex.gz` data.
They are unavailable for remote vaults and for PDFs whose pages you have rearranged since building.

## Lean

Open a `.lean` file in a Lean project with `lake serve` available.
The Code pane shows goals and the expected type at the caret, while a bar in the gutter shows checking progress.
Type an abbreviation such as `\to`, then press `Tab` to insert `→`.
These abbreviations also work while the server is still starting.
After rebuilding imports, run **Restart File in Language Server** from the [command palette](keyboard.md).
