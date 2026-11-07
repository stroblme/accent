# Bibliographies

Accent manages BibTeX and BibLaTeX references in ordinary `.bib` files.
The **Bibliography** pane appears when one is found in the vault.
Choose the file at the top if there are several, or run **New Bibliography** from the command palette to create one.
Opening a `.bib` tab shows its source and selects it in the pane.

## Add and edit references

Choose **Add Reference** to enter a DOI or DOI URL, paste BibTeX, or fill in an entry manually.
Review the metadata before adding it.
The **Download PDF** checkbox optionally tries to attach a PDF as the reference is added.
A failed download keeps the reference, and **Attach PDF** can import a file downloaded elsewhere.

Click a reference to find it in the source, or use **Edit Reference** from its row menu to edit fields.
The source and the pane share autosave and Undo.
Accent preserves unrelated fields, comments and formatting.
If a file has syntax errors, edit its source before using structured changes.

## Cite, check and share

Use a reference's row menu to copy its citation key, refresh metadata, open its PDF or find usages.
**Insert Citation** writes `\cite{keys}` into the active LaTeX document and `@key` for each key into a Typst document.
**Copy Citation Keys** also works when writing in another application.

The pane's **More** menu offers **Check** and **Merge Duplicates**.
Checks identify missing fields, duplicate keys, malformed DOIs and broken attachments.
Renaming or merging a key used in another indexed document stops so you can update its citations first.

**Copy to Bibliography…** and **Move to Bibliography…** transfer selected references to another `.bib` file, including their inheritance dependencies.
Copies are independent, and attachment paths are adjusted for the destination.
Removing a reference keeps its attachment files.
These management controls are available on desktop, including remote vaults.
