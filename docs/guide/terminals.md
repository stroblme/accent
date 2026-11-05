# Terminals

Press `Ctrl+J` to open a terminal tab at the [vault](vaults.md) root.
Without a vault, it opens in your home folder.
Terminals can share the same splits and tab controls as your documents.
Right-click a folder in Files and choose **Open in Terminal** to start in that folder.
In the Flatpak, local shells run on your computer with its installed tools.

Closing a terminal tab with `Ctrl+W` ends its shell.
Closing the window leaves the shell running, and reopening the vault restores its tab and screen.
Use `Ctrl+Shift+C` and `Ctrl+Shift+V` to copy and paste in a terminal.
Most other `Ctrl` keys go to the shell, so commands such as `Ctrl+C` work as expected.

In a [remote vault](remote.md), **New Terminal** opens on the host.
Choose **New Local Terminal** for a shell on your computer, or **New Remote Terminal…** to connect to another host.
If a remote connection drops, press a key in its terminal tab to reconnect and recover the screen.

## Terminal editors

Under **Preferences → Editor → Text Editor**, choose Accent or an installed Helix, Neovim, Vim, Kakoune, Emacs, Nano or Micro.
Text files then open in that editor in terminal tabs named after their files.
PDFs, images, diagrams and notebooks keep their own views.
Read-only dependency files stay in Accent's editor.
Search hits, symbols, definitions and references open at their line, with a column where available.
Opening a file already in a terminal editor brings its tab forward without moving its cursor.

The editor runs where the file lives, including on a remote vault's host, where it must also be installed.
The chooser detects local programs, and a remote host uses the editor's usual command name, such as `hx` for Helix.
If that command is missing, the file opens in Accent's editor with a message.
Closing the window preserves a running editor with the vault's session.
Closing its kept tab asks before ending it and losing any unsaved changes.
Use **Open in Accent's Editor** from its tab menu to switch back for one file.

While the terminal editor has focus, `Ctrl+W`, `Ctrl+E` and `Ctrl+J` reach it.
Use the tab controls or the command palette for Accent's corresponding actions.
Preview, Outline and References need Accent's own editor.
Helix starts with a theme using the terminal's colours by default.
Set `editor.true-color = true` in Helix's configuration to use a truecolor theme.

See [Preferences](preferences.md#commands-and-terminals) for saved commands that run in terminal tabs.
