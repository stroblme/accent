# Accent

<p align="center">
<img src="https://raw.githubusercontent.com/stroblme/accent/refs/heads/main/data/icons/logo.svg" width="200" title="Logo">
</p>
<br/>

An opinionated text editor which can serve as a knowledge management system and IDE in one software as a result of my personal frustration with other software.
The core engine is written in [Rust](https://rust-lang.org/) ensuring that Accent never takes longer than a second to start and stays stable even when being faced with huge vaults.
UI and UX focus on efficiency paired with a aggressively minimal design aimed to remove all the clutter.

Some features:
- Vault management with templates, tags and links
- PDF viewer and editor with Pen support
- Light/Dark/Solarized theming, including images and PDFs
- Language server and autocompletion based on [Merl](https://github.com/stroblme/merl)
- Multi-caret selection and edit
- Customizable shortcuts and command palette
- Git management and diff. view for resolving file conflicts
- Remote vaults via dedicated ssh server
- Integrated terminal and terminal emulator (tmux-like)
- Multi-pane support, Android app and much more

This project was formally known as "UNote" (PDF Editor) and is now rewritten from scratch.

## Installation

The current implementation is focused on Linux/openSUSE (Gnome) and Android support.
That being said, other distros and desktops will very likely work.
Adding Windows or Mac support is a thing that will not happen any time soon (sorry).

```
make requirements      # names what is missing, see Requirements below
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

### Requirements

The desktop app needs Rust ≥ 1.92 ([rustup](https://rustup.rs)), a C compiler, pkg-config, and the
development files of GTK ≥ 4.18, libadwaita ≥ 1.7, GtkSourceView ≥ 5.18, WebKitGTK 6.0, VTE for
GTK 4 ≥ 0.78 and libspelling. Debian 13 and Ubuntu 24.04 are too old for these; Debian testing,
Ubuntu 26.04, Fedora 43, Arch and Tumbleweed have them all.

Optional, each for one feature: libpdfium for PDFs (`make pdfium`, which uses `curl`), `git` for
the Git pane, `ssh` and `make server` for remote vaults, `merl-rt` from
[merl](https://github.com/stroblme/merl) for ghost text, and a language server per programming
language (the app names the one it misses). `make requirements` checks all of this but the
language servers, and installs nothing. `accent-cli`, which the builds above put beside `accent`,
keeps a terminal's shell running when its window closes.

```
# openSUSE Tumbleweed
sudo zypper install gcc pkgconf-pkg-config glib2-devel gtk4-devel libadwaita-devel \
  gtksourceview5-devel webkitgtk4-devel vte-devel libspelling-devel git-core openssh-clients curl
# Fedora 43+
sudo dnf install gcc pkgconf-pkg-config glib2-devel gtk4-devel libadwaita-devel \
  gtksourceview5-devel webkitgtk6.0-devel vte291-gtk4-devel libspelling-devel git openssh-clients curl
# Debian testing, Ubuntu 26.04+
sudo apt install build-essential pkgconf libgio-2.0-dev-bin libgtk-4-dev libadwaita-1-dev \
  libgtksourceview-5-dev libwebkitgtk-6.0-dev libvte-2.91-gtk4-dev libspelling-1-dev git openssh-client curl
# Arch
sudo pacman -S --needed base-devel gtk4 libadwaita gtksourceview5 webkitgtk-6.0 vte4 libspelling \
  git openssh curl
```

## Roadmap

- [ ] Flatpak install
- [ ] Debugger (DAP client)
- [ ] MCP server
- [ ] Bibtex library management
- [ ] Export and printing notes
- [ ] Pasting/ dropping images into .md files

## License & References

Accent is licensed under GPL-3.0-or-later.

References and inspiration from: [Apostrophe](https://github.com/ApostropheEditor/Apostrophe), [Obsidian](https://obsidian.md/), [VSCodium](https://code.visualstudio.com/)
Icons from: [Tabler](https://github.com/tabler/tabler-icons)