# The workspace

Each vault is a folder of files, and Accent gives it its own window.
Open one with **Open Local…** from the menu, or use **Open Remote…** for a folder over SSH.
The start screen lists recent vaults, so picking up where you left off is quick.
See [Remote hosts](remote.md) if your files live on another machine.

## Files and tabs

The **Files** pane shows the vault's folders and files.
Choose **New File** (`Ctrl+N`) or **New Folder** (`Ctrl+Shift+N`) to add one.
To make a note, give the file a `.md` ending, such as `Journal/Wednesday.md`.
You can type `/` in a new file's name, and Accent will make the folders along the way.
The [Notes](notes.md) page covers links, attachments and templates.

Click a file to open a temporary preview tab.
Click another and it replaces that tab, which keeps a quick browse from filling the tab bar.
Edit the file, double-click its tab, or click its eye to keep it open.
You can also pin a tab from its menu.
Drag a tab or a file to an edge of the document area to open a split pane.
The active pane is the one you last worked in, and `Ctrl+Tab` walks its recently used tabs.

Hidden files appear dimmed by default, and you can hide them under **Preferences → Files**.
Ignored folders still appear in the tree, but [search](search.md) skips most of their files by default.
Markdown notes remain searchable even there.
You can open a file from outside the vault with **Open File…** (`Ctrl+O`), though it will not have vault backlinks.

## Find your way

Use the sidebar's pane icons to switch between **Files**, **Search**, **Info** and **Outline**.
Other panes appear when they apply, such as [Git](git.md), [remote ports](remote.md) or a diagram's **Properties**.
Press `F9` to show or hide the sidebar.
Press `Ctrl+E` to find a file by name, or `Ctrl+P` to run a command by name.
In the same palette, start with `#` for tags or `@` for symbols.
The [Keyboard shortcuts](keyboard.md) page has a small cheat sheet.

Images open fitted to the window, and you can zoom with `Ctrl++` or `Ctrl+-`.
For scans and screenshots, Accent can recolour the image to match the theme.
You can adjust that in [Preferences](preferences.md).
For other document types, see [PDFs](pdf.md), [diagrams](diagrams.md), [notebooks](notebooks.md) and [code](code.md).
