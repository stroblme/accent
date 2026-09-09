# Accent

An opinionated text editor which can serve as a knowledge management system and IDE in one software as a result of my personal frustration with other software.
The core engine is written in [Rust]() ensuring that Accent never takes longer than a second to start and stays stable even being faced with huge vaults.
UI and UX focus on efficiency paired with a aggressively minimal design aimed to remove all the clutter.

Some features:
- Vault management with templates, tags and links
- PDF viewer and editor with Pen support
- Language server and autocompletion based on [Merl]()
- Git management and diff. view for resolving file conflicts
- Remote vaults via dedicated ssh server
- Integrated terminal
- Multi-pane support, Android app and much more

TOOD: some screenshots or gif


This project was formally known as "UNote" (PDF Editor) and is now rewritten from scratch.
Accent can do everything UNote did (and much more) and I didn't saw a reason for having two times the same app.

## Installation

### The lazy way

TODO: flatpak install and one-line install bash command

### Build your own

```
cargo build            # core + api + cli
cargo test
cargo build -p accent  # GTK app
cargo run --release -p accent -- testvault
```

## Architecture

TODO: brief description of the architecture and software stack

## License & References

Accent is licensed under GPL-3.0-or-later.

References and inspiration from: [Apostrophe](), [Obsidian](), [VSCodium]()
Icons from:
Other references: