# Debugging Python

Install Python and `debugpy` on the computer holding your [vault](vaults.md).
Accent uses the nearest `.venv` or `venv` for your program, or `python3` if neither exists.
If your interpreter lives elsewhere, set it in `~/.config/accent/config.toml`:

```toml
[debug]
python = ["env/bin/python"]
```

The path in this example is relative to the vault root.

## Start and stop

Open a Python file and press `Ctrl+F5` to start debugging it.
Accent saves your open changes first, then opens the program in a terminal tab.
Click the editor gutter to add or remove a breakpoint, or press `Ctrl+F9` on its line.
Breakpoints return when you reopen the vault.

The Code pane shows the call stack, variables, watches, and breakpoints when the program pauses.
Pick a stack frame to inspect it, or add an expression in Watch to see its value at each stop.
Hover over a variable in the editor to see its value.

Use `F5` to continue, `F6` to pause, and `F10`, `F11`, or `Shift+F11` to step over, into, or out.
Press `Shift+F5` to stop or `Ctrl+Shift+F5` to restart.
The same controls are in the Code pane and the [command palette](keyboard.md).

Open **Debug Console** from the Code pane to see adapter messages and evaluate Python expressions.
Expressions run in the selected stack frame while the program is paused.
Your program's own input and output stay in its terminal tab.

On a [remote vault](remote.md), install `debugpy` on the host so the program runs there.
The Flatpak cannot debug a local vault yet, though remote debugging works.
