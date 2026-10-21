# Notebooks

Open a Jupyter `.ipynb` file or a marimo notebook from your [vault](vaults.md).
Accent shows its code and Markdown as editable cells, with saved outputs below them.
You can add, move, split, join, convert, or delete cells from their buttons and **More** menu.
Undo and Redo cover cell changes as well as typing.

## Run Jupyter cells

Install `ipykernel` in the Python environment used by the notebook.
Accent uses the nearest `.venv` or `venv`, or `python3`, and starts a kernel on the first run.
The notebook must be inside a vault to run.

Select a code cell and press `Ctrl+Return` to run it.
`Shift+Return` runs it and selects the next cell, adding one at the end if needed.
The Code pane has **Run All**, **Interrupt Kernel**, **Restart Kernel**, and **Shut Down Kernel**.
Outputs and execution counts are saved with the notebook, so they are still there next time.

Code cells also use the [code editor's](code.md) completion, diagnostics, hover, and Go to Definition.
The Outline and [search](search.md) help you move between cells.
Marimo notebooks can be edited here, but run through marimo itself.
