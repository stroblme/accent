#!/bin/sh
# A stand-in for a terminal editor in the `ACCENT_BENCH_EDITOR` drills, which never run a real
# one: linked into a scratch HOME's bin as `nano`, it counts its starts, says what it was given,
# reads a line and ends with that line as its exit code, the file it was given told `edited`.
# Linked as `kak`, it ends as a program the shell cannot find does. Under
# `ACCENT_FAKE_EDITOR=keys` it logs the next three bytes it reads, raw, as hex.
# The drills take the line it says for the editor being up, so it comes after the count and the
# switch to raw input, never before.
[ "$(basename "$0")" = kak ] && exit 127
echo $$ >>"$HOME/editor-starts"
if [ "$ACCENT_FAKE_EDITOR" = keys ]; then
    stty raw -echo
    echo "fake-editor $*"
    dd bs=1 count=3 2>/dev/null | od -An -tx1 >"$HOME/editor-keys"
    stty sane
    exit 0
fi
echo "fake-editor $*"
for file; do :; done
read -r code
echo edited >>"$file"
exit "$code"
