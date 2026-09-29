# Accent

<p align="center">
  <img src="https://raw.githubusercontent.com/stroblme/accent/refs/heads/main/data/icons/logo.svg" width="160" alt="Accent logo">
</p>

Accent is a home for Markdown notes and PDFs. Keep your work in ordinary folders, connect ideas with links and tags, and read them on Linux or Android. Both native apps share a Rust core.

- Write and search notes, use templates, and browse links and tags.
- Read and mark up PDFs; link highlighted passages back to your notes.
- Make the desktop your own with split panes, themes, shortcuts, and a command palette.
- Work with Git, language servers, terminals, and vaults on remote hosts over SSH.

Accent is open source under GPL-3.0-or-later and still evolving.

## Get started

On Linux, install the [desktop requirements](#desktop-requirements), then run:

```sh
make requirements    # check local dependencies
make pdfium          # optional, for PDFs
make all             # build the GTK app and accent-cli
target/release/accent /path/to/notes
```

You can also launch `target/release/accent` without a path and choose a folder in the app. `accent --help` lists the other launch forms: a file such as a PDF, a remote vault, a terminal. `make server` builds the static helper needed for remote vaults.

For Android, install a JDK, the Android SDK and NDK, then build a debug APK:

```sh
make android-tools
make apk
```

The Android app lives in [`android/`](android/) and uses the same Rust core through UniFFI.

### Desktop requirements

The desktop build needs [Rust](https://rustup.rs) ≥ 1.92, a C compiler, pkg-config, and development files for GTK ≥ 4.18, libadwaita ≥ 1.7, GtkSourceView ≥ 5.18, WebKitGTK 6.0, VTE for GTK 4 ≥ 0.78, and libspelling. `make requirements` reports what is missing and installs nothing. Git, SSH, [Merl](https://github.com/stroblme/merl), and language servers enable their corresponding features.

<details>
<summary>Package examples for Linux distributions</summary>

```sh
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

</details>

Accent takes cues from [Apostrophe](https://github.com/ApostropheEditor/Apostrophe), [Obsidian](https://obsidian.md/), and [VSCodium](https://vscodium.com/). Icons come from [Tabler](https://github.com/tabler/tabler-icons).
