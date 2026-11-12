#!/bin/sh
# Fetches draw.io's stencil packs, the XML files that draw a diagram's `shape=mxgraph.…` shapes,
# into the folder accent reads them from.
#
# accent does not ship them, since they are 43 MB of optional third-party data (Apache-2.0, and
# their LICENSE comes along), and a diagram opens without them, with such shapes as placeholders.
# Only `src/main/webapp/stencils` of draw.io is fetched, at a pinned commit, through a blobless
# sparse checkout, so the download is the packs and not the whole repository.
#
# Usage: build-aux/stencils.sh [dest]
# dest defaults to $XDG_DATA_HOME/accent/stencils, and the script does nothing when it holds them.
set -eu
# draw.io v31.4.5, checked after the fetch.
repo=https://github.com/jgraph/drawio
commit=f3abfe0f082c18f7b4fee8a34c2d07b1987687fd
dest=${1:-${XDG_DATA_HOME:-$HOME/.local/share}/accent/stencils}

if [ -f "$dest/aws4.xml" ]; then
	echo "The stencil packs are already in $dest."
	exit 0
fi
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
git -C "$tmp" init -q
git -C "$tmp" sparse-checkout set --no-cone /src/main/webapp/stencils/
git -C "$tmp" fetch -q --depth 1 --filter=blob:none "$repo" "$commit"
fetched=$(git -C "$tmp" rev-parse FETCH_HEAD)
if [ "$fetched" != "$commit" ]; then
	echo "Fetched $fetched instead of draw.io's $commit." >&2
	exit 1
fi
# The checkout downloads the blobs the sparse pattern keeps, and only those.
git -C "$tmp" checkout -q FETCH_HEAD
mkdir -p "$dest"
cp -R "$tmp/src/main/webapp/stencils/." "$dest"
echo "The stencil packs are in $dest."
