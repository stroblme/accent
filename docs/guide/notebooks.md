# Notebooks

Open a Jupyter `.ipynb` file or a marimo notebook from your [vault](vaults.md).
Accent shows its code and Markdown as editable cells, with saved outputs below them.
You can add, move, split, join, convert, or delete cells from their buttons and **More** menu.
Undo and Redo cover cell changes as well as typing.

## Run Jupyter cells

Install `ipykernel` in the Python environment used by the notebook.
Choose its interpreter from **Python Environment** in the Code pane.
This choice is kept for the vault and also applies to Python debugging and language analysis.
Accent starts a kernel on the first run, in the notebook's folder.
The notebook must be inside a vault to run.

Select a code cell and press `Ctrl+Return` to run it.
`Shift+Return` runs it and selects the next cell, adding one at the end if needed.
The Code pane has **Run All**, **Interrupt Kernel**, **Restart Kernel**, and **Shut Down Kernel**.
While a cell runs, its Run button becomes **Stop**, which interrupts the kernel.
Outputs and execution counts are saved with the notebook, so they are still there next time.

For another language, such as R or Julia, install a Jupyter kernelspec for it where the vault lives and choose it from **Kernel** in the Code pane.
The kernel must support Jupyter's `ipc` transport.
Accent keeps that choice for the notebook without changing its metadata.

Code cells also use the [code editor's](code.md) completion, diagnostics, hover, and Go to Definition.
Markdown cells offer note links, completion, spelling and ghost text, although their links and tags do not contribute to the vault's index or link updates.
The Outline and [search](search.md) help you move between cells.
Marimo notebooks can be edited here, but run through marimo itself.

## Copy and export

Right-click an output and choose **Copy Output** to copy a picture as an image, or other output as plain text.
The notebook's file menu offers **Print…** and **Export as HTML…** with cell sources and saved outputs.
Printing opens the page in your browser, where the print dialog can save a PDF.
Interactive widgets appear as a description of the output.
