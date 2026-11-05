# Search and info

Press `Ctrl+Shift+F` to search across the vault.
Results include notes, code and other indexed text, plus labels in diagrams, sources in notebooks and the text of PDFs.
Choose a result to open its file.

## Narrow a search

With the search options off and the replace field closed, words match in any order.
Put a phrase in double quotes to keep its words together.
Use **Filter** beside the query to insert a qualifier, or type one directly.

| Qualifier | Finds |
| --- | --- |
| `#research` or `tag:research` | Notes with that tag or a nested tag |
| `tag:research,ideas` | Notes with either tag |
| `path:"Project notes"` | Files whose vault path contains that text |
| `kind:pdf` | PDFs, with `note`, `diagram`, `notebook`, `code`, `text` and `image` also available |
| `links:[[Project]]` | Notes linking to Project, including links waiting for it to be created |
| `linked-from:[[Project]]` | Files Project links to |
| `has:highlights` | Notes linking to PDF passages and the PDFs they quote |
| `modified:7d` | Files modified in the last seven days, with `12h` and `2w` also accepted |
| `modified:2026-11-01` | Files modified on that day, with `a..b` for a span of days |

Combine words and qualifiers, for example `quantum #research kind:pdf,note -path:Archive`.
All conditions must match, while comma lists in `tag:` and `kind:` accept either value.
A leading `-` excludes a word or qualifier, such as `-draft` or `-#read`.
There is no `OR` operator or grouping with parentheses.
Qualifiers alone list matching files, newest first.

## PDF results

A PDF is listed once for each page that matches, with the page number, such as `p. 12`.
Choosing it opens the PDF on that page with the match highlighted; press `F3` for the next match.
After a vault opens, Accent reads the text of its PDFs in the background and shows its progress in the status bar, where **Stop** ends it until you open the vault again.
Scanned PDFs without a text layer are not searchable.

## Exact search and replacement

The buttons under the query let you match case (`Aa`), whole words (`Word`) or a regular expression (`.*`).
Turning on any of them, or opening the replace field, searches the query as text and disables **Filter**.
Use **All** to search ignored folders too.
These extra results appear after the indexed ones, and **Replace All** does not change them.
For a ranked search, this extra walk needs a single word or phrase with no qualifiers.
To replace across indexed files, open the replace field and review the count on **Replace All** before confirming.

For a single open document, press `Ctrl+F` instead.
Type to find, then use `Enter` and `Shift+Enter` to move between matches.
`Ctrl+H` opens replacement in the current document.
The find bar belongs to its pane, so each half of a split can have a different search.

## Saved searches

Put a query in a `search` fence in a note to list its results in the desktop preview.

````markdown
```search limit=10
#research -#read kind:note,pdf
```
````

The list updates when the vault changes and leaves the note holding the query out of its results.
Choose a row to open the file, or its matching PDF page.
Without `limit=`, it shows up to 20 rows, with 100 as the maximum.
Org notes use `#+begin_src search` and `#+end_src` around the same query.
Printouts and HTML exports show the query under a Saved search caption.
On [Android](android.md), **Show Results** opens the query in Browse.

## Info and outline

The **Info** pane (`Ctrl+Shift+B`) brings related details together.
**Links** shows notes that link to the current file, and its arrow button switches a note's list to the files it links to.
**Tags** lists tagged notes in the vault.
Open **Details** there for facts such as a note's word count or a PDF's page count.
The **Outline** pane (`Ctrl+Shift+W`) lists headings in the open note, so a long page is easier to navigate.
For names of files instead of their contents, use **Go to File** (`Ctrl+E`) in [the workspace](vaults.md#find-your-way).
