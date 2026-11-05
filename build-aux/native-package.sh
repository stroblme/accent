#!/usr/bin/env bash
# Builds accent's .deb or .rpm in a container of the distribution it is for, so the package names
# each library it needs as that distribution does. Then it installs the package in a fresh
# container of the same image and starts both binaries, which fails on any library or symbol the
# package does not pull in (Rust links with -z now).
#
# Run it from the repository root with `make server`'s binaries in target/. The package's
# extension picks the format:
#     build-aux/native-package.sh fedora:44 accent-fedora-44-x86_64.rpm
set -euo pipefail
image=$1 package=$2
format=${package##*.}

# Just enough for `make install-requirements` and the packaging tool; that target brings the rest.
case $image in
ubuntu:* | debian:*)
    setup="apt-get update && apt-get install -y make"
    install="apt-get update && apt-get install -y" ;;
fedora:*)
    setup="dnf install -y make rpm-build"
    install="dnf install -y" ;;
opensuse/*)
    setup="zypper --non-interactive install make tar gzip rpm-build"
    install="zypper --non-interactive --no-gpg-checks install --allow-unsigned-rpm" ;;
*)
    echo "No package manager known for $image"
    exit 1 ;;
esac

docker run --rm -v "$PWD:/src" -w /src "$image" bash -euc "
    $setup
    make install-requirements
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --quiet --profile minimal
    . \$HOME/.cargo/env
    make $format
    find target/release -maxdepth 4 -name 'accent*.$format' -exec mv {} '$package' ';'"

docker run --rm -v "$PWD:/src" -w /src "$image" bash -euc "
    $install ./'$package'
    accent --help >/dev/null
    accent-cli --help >/dev/null"
