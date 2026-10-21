# Welcome to Accent

Accent keeps notes, code, PDFs and drawings together in an ordinary folder called a **vault**.
Your files stay yours, and you can edit them with other apps too.
The desktop app runs on Linux, with an [Android companion](android.md) for notes on the go.

## Install and open a vault

Download the Flatpak bundle from the [latest release](https://github.com/stroblme/accent/releases/latest), then install it with:

```sh
flatpak install --user accent-<version>-x86_64.flatpak
```

For a native install on a recent GNOME Linux system, use the [installer](https://github.com/stroblme/accent/blob/main/install.sh):

```sh
curl -fsSL https://raw.githubusercontent.com/stroblme/accent/main/install.sh | bash
```

Open Accent and choose **Open Local…** to use a folder you already have, or make a new one in the folder chooser.
Accent will show its files in the sidebar and start indexing them for [search](search.md) and links.
The files remain ordinary files in that folder.

You can also open a folder from a terminal with `accent ~/Notes`.
See [The workspace](vaults.md) for the window, tabs and file tree, or start with [your first note](notes.md).
For a folder on another machine, see [Remote hosts](remote.md).
