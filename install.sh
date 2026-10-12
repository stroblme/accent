#!/usr/bin/env bash
set -euo pipefail

REPO=stroblme/accent

ensure_rust() {
    local found
    found=$(rustc --version 2>/dev/null | cut -d' ' -f2) || true
    if command -v cargo >/dev/null && printf '%s\n' "$2" "$found" | sort -VC; then
        return
    fi
    echo "Installing a temporary Rust toolchain (Rust >= $2 needed, found ${found:-none})"
    export RUSTUP_HOME=$1/rustup CARGO_HOME=$1/cargo
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
        sh -s -- -y --quiet --profile minimal --no-modify-path >/dev/null
    export PATH=$CARGO_HOME/bin:$PATH
}

main() {
    if [ "$(id -u)" = 0 ]; then
        echo "Run this as your own user: it installs into ~/.local and asks sudo only for packages."
        exit 1
    fi
    if ! command -v make >/dev/null; then
        echo "accent builds with make: install it with your package manager, then run this again."
        exit 1
    fi
    local ref=${ACCENT_REF:-}
    if [ -z "$ref" ]; then
        # The newest version tag; each one is a release.
        ref=$(curl -fsSL "https://api.github.com/repos/$REPO/tags?per_page=100" |
            sed -n 's/.*"name": *"\(v[0-9][^"]*\)".*/\1/p' | sort -V | tail -n 1)
    fi
    # On disk rather than in /tmp: a release build takes several gigabytes.
    local cache=${XDG_CACHE_HOME:-$HOME/.cache}
    mkdir -p "$cache"
    work=$(mktemp -d "$cache/accent-install.XXXXXX")
    trap 'rm -rf "$work"' EXIT

    echo "Building accent $ref"
    mkdir "$work/accent"
    curl -fsSL "https://github.com/$REPO/archive/$ref.tar.gz" |
        tar -xz --strip-components=1 -C "$work/accent"
    cd "$work/accent"

    ensure_rust "$work" "$(sed -n 's/^RUST_MIN *:= *//p' Makefile)"
    if ! make --no-print-directory requirements; then
        make --no-print-directory install-requirements
        make --no-print-directory requirements || {
            echo "This distribution's GNOME libraries are older than accent needs (GNOME 49 or"
            echo "newer: Fedora 43+, openSUSE Tumbleweed, Debian testing, Ubuntu 26.04+, Arch)."
            echo "The Flatpak from https://github.com/$REPO/releases runs anywhere."
            exit 1
        }
    fi
    make --no-print-directory pdfium || echo "No libpdfium for this machine: PDFs will not open."
    make --no-print-directory install
}

main "$@"
