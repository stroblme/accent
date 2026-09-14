# Accent

An opinionated text editor which can serve as a knowledge management system and IDE in one software as a result of my personal frustration with other software.
The core engine is written in [Rust]() ensuring that Accent never takes longer than a second to start and stays stable even when being faced with huge vaults.
UI and UX focus on efficiency paired with a aggressively minimal design aimed to remove all the clutter.

Some features:
- Vault management with templates, tags and links
- PDF viewer and editor with Pen support
- Language server and autocompletion based on [Merl]()
- Git management and diff. view for resolving file conflicts
- Remote vaults via dedicated ssh server
- Integrated terminal
- Multi-pane support, Android app and much more (see [Roadmap](#Roadmap))

TOOD: some screenshots or gif


This project was formally known as "UNote" (PDF Editor) and is now rewritten from scratch.
Accent can do everything UNote did (and much more) and I didn't saw a reason for having two times the same app.

## Installation

The current implementation is focused on Linux/openSUSE (Gnome) and Android support.
That being said, other distros and desktops will very likely work.
Adding Windows or Mac support is a thing that will not happen any time soon (sorry).

TODO: flatpak install and one-line install bash command

```
cargo build            # core + api + cli
cargo test
cargo build -p accent  # GTK app
cargo run --release -p accent -- testvault
```

The Android app lives in `android/` and shares the same Rust core through uniffi bindings. It
needs a JDK, the Android SDK and NDK, and `make android-tools` for the rest:

```
make android-tools     # rust targets + cargo-ndk
make pdfium-android    # libpdfium for each ABI
make apk               # cross-builds the core, generates the bindings, packs the APK
```

## Roadmap

- [ ] Debugger (DAP client)
- [ ] MCP server
- [ ] Bibtex library management
- [ ] Export and printing notes
- [ ] Pasting/ dropping images into .md files
- [ ] Persistent terminals

## License & References

Accent is licensed under GPL-3.0-or-later.

References and inspiration from: [Apostrophe](), [Obsidian](), [VSCodium]()
Icons from: [Tabler](https://github.com/tabler/tabler-icons)