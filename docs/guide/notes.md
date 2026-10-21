# Notes

A note is a Markdown file ending in `.md`.
Write in the editor, and Accent saves your changes after a short pause.
Headings, lists, tables, task lists, footnotes and math work in notes.
For example, `- [ ] Pack a notebook` makes a task, while `$x^2$` adds inline math.

## Links and tags

Use `[[Trip plan]]` to link to another note, or `[[Trip plan#Packing]]` to link to its heading.
Add a label with `[[Trip plan|our plan]]`.
If the note does not exist yet, following the link offers to create it.
Accent suggests notes as you type `[[`, and you can browse links back to the current note in [Info](search.md#info-and-outline).

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

Press `Ctrl+M` to show the rendered preview beside the editor, or return to the editor.
The preview follows your edits and lets you follow links and tags.
Use **Print…**, **Export as PDF…** or **Export as HTML…** from the menu when you want to share a note.
The HTML export includes local images, so it works as one file.
If `merl-rt` is installed, Accent can suggest the rest of a line as faint **ghost text**.
Press `Tab` to accept it, `Ctrl+Right` to take one word, or `Esc` to dismiss it.
Turn **Ghost Text** on or off under [Preferences → Editor](preferences.md#editor-and-files).
