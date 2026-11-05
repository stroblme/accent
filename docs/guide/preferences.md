# Preferences

Open **Preferences** from the main menu or with `Ctrl+,`.
Changes take effect in every open window.

## Appearance

Choose **System**, **Light**, **Dark** or **Solarized** under **Theme**.
Solarized follows the system's light and dark setting.
**Focus Mode** decides how much of the window fades while you write.
Choose **None** to keep everything visible, **Medium** to fade the bars and sidebar, or **High** to fade text away from the caret and other panes too.
Moving the pointer or pressing `Esc` brings the controls back.
You can also change how images, PDFs and diagrams are recoloured to match the theme.

## Editor and files

**Text Editor** chooses Accent or a [terminal editor](terminals.md#terminal-editors) found by your login shell.
**Document Font** changes the editor's font, while **Preview Font** changes the rendered page and its printouts and HTML exports.
Until you choose one, the editor uses the system monospace font and the preview uses the system document font.
**Column Width** sets how wide the text can grow, and **Full-Width Preview** gives wide tables more room.
The editor also offers **Spell Checking**, **Word Suggestions**, **Ghost Text**, **Line Numbers** and a **Minimap**.
See [Notes](notes.md) for ghost text and preview use.

Under **Files**, **Show Hidden Files** controls names beginning with a dot.
**Colour Changed Files** tints files changed in Git and their folders in the [Files pane](vaults.md#files-and-tabs).

## Commands and terminals

Under **Commands**, choose **Add Command** to save a name, command line and optional shortcut.
Run it by name from the command palette or with its shortcut.
It opens in a terminal at the vault root, on the host for a remote vault, and closes after a successful exit.
An unsuccessful exit keeps the tab and its output visible.
Closing the window leaves a running command available when the vault reopens.

Under **Terminal**, **Debug Terminal** places a debugged program's terminal in the same pane or a split on the chosen side.
It leaves the keyboard in the source editor.

## This Vault

Set **Templates Folder** for the templates used by [notes](notes.md#templates).
**New Files Folder** is where **New File** starts when no folder is selected in the tree.
**Attachments Folder** is where pasted and dropped images go.
Leave it empty to store images beside the note, use `./images` for a subfolder beside it, or use `Images` for a folder at the vault root.
