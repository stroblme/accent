# Notes

A note is a Markdown file ending in `.md`.
Write in the editor, and Accent saves your changes after a short pause.
Headings, lists, tables, task lists, footnotes and math work in notes.
For example, `- [ ] Pack a notebook` makes a task, while `$x^2$` adds inline math.
Press `Alt+X` to cycle a task through open `[ ]`, in progress `[/]` and done `[x]`.
An ordinary list item becomes a task on the first press.

## Links and tags

Use `[[Trip plan]]` to link to another note, or `[[Trip plan#Packing]]` to link to its heading.
Add a label with `[[Trip plan|our plan]]`.
If the note does not exist yet, following the link offers to create it.
Accent suggests notes as you type `[[`, and you can browse the links to and from the current note in [Info](search.md#info-and-outline).

Add `#travel` in the text to tag a note.
You can also put tags and alternative names in front matter at the top:

```yaml
---
tags: [travel, ideas]
aliases: [Summer trip]
---
```

Search for an alias in **Go to File**, or pick a tag from the **Info** pane.
See [Search](search.md) for both.

Paste an image or drop one into a note to copy it into the vault and insert an embed.
You can also write `![[map.png]]` by hand.
Set the destination under **Preferences → This Vault → Attachments Folder**.

## Org files

Accent opens an [Org mode](https://orgmode.org) file (`.org`) as a note, with styled markup and spell checking.
Its headings fill the **Outline** and fold, and its links work as a note's do: `[[file:Trip plan.md]]` or `[[./trip.org::*Packing]]` reaches a file, `[[*Packing]]` a heading in the same file, and `F12` or `Ctrl`+click follows one.
A note reaches an Org file as `[[trip]]`; a note of the same name wins.
Heading tags such as `:travel:` and `#+FILETAGS` are listed with the note tags, `#+TITLE` names the file in search, and moving or renaming a file updates the links in Org files too.
`Ctrl+M` opens its preview, and its file menu offers printing and HTML export as for Markdown.
The rendered page includes lists, tables, source blocks, quotes, footnotes, images and math.
Drawers, planning lines and most keyword lines stay out of the page, and source blocks are displayed without execution.
`[[id:…]]` links are not followed.
Use `Alt+X` to cycle checkboxes through `[ ]`, `[-]` and `[X]`, or to cycle a heading's TODO state.
`Tab` aligns a table and moves to the next cell.

## Templates

Put `.md` files directly in the vault's **Templates Folder**, which is `Templates/` by default.
When you create a note with **New File**, choose a template to fill it in.
**Insert Template…** puts any template at the caret of an open note.
You can use `{{title}}`, `{{date}}`, `{{time}}` and `{{cursor}}` in the template.
The first `{{cursor}}` sets the caret, and `Tab` moves through any later ones.

Give a template its own destination to use **New from Template…** (`Ctrl+Shift+D`):

```markdown
---
accent-target: Daily/{{date:%Y-%m-%d}}.md
---
# {{date}}

{{cursor}}
```

Running this command again on the same day opens the existing note without changing it.
For a different day, `{{date-1}}` means yesterday, and `{{date+1}}` means tomorrow.
The `accent-target` line is only an instruction to Accent, so it does not appear in the new note.

## Preview and export

Press `Ctrl+M` to open the rendered preview in a tab beside the note, or switch between that preview and its editor.
Drag the preview into a split pane to read it alongside the editor.
The preview follows your edits and lets you follow links and tags.
Markdown callouts such as `> [!note] A useful detail` render as titled blocks.
On the desktop, Mermaid fences show their source.
Use a [`search` fence](search.md#saved-searches) to show live vault results in the preview.
Right-click a note's tab or its row in the Files pane and choose **Print…** or **Export as HTML…** when you want to share it.
The HTML export includes local images, so it works as one file.
Printing opens the rendered page in your system browser, whose print dialog can also save a PDF.
Accent suggests the rest of a line as faint **ghost text**, from what your vault has written before.
Press `Tab` to accept it, `Ctrl+Right` to take one word, or `Esc` to dismiss it.
Turn **Ghost Text** on or off under [Preferences → Editor](preferences.md#editor-and-files).
The suggestions come from [merl](https://github.com/stroblme/merl), which is built into Accent.
To try a merl build of your own, set `ghost = ["/path/to/merl-rt"]` under `[lsp]` in `~/.config/accent/config.toml`.
