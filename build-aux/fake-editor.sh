#!/bin/sh
# A stand-in for a terminal editor in the `ACCENT_BENCH_EDITOR` drills, which never run a real
# one: linked into a scratch HOME's bin as `nano`, it says what it was given, counts its starts,
# reads a line and ends with that line as its exit code, the file it was given told `edited`.
# Linked as `kak`, it ends as a program the shell cannot find does. Under
# `ACCENT_FAKE_EDITOR=keys` it logs the next three bytes it reads, raw, as hex.
[ "$(basename "$0")" = kak ] && exit 127
echo "fake-editor $*"
echo $$ >>"$HOME/editor-starts"
if [ "$ACCENT_FAKE_EDITOR" = keys ]; then
    stty raw -echo
    dd bs=1 count=3 2>/dev/null | od -An -tx1 >"$HOME/editor-keys"
    stty sane
    exit 0
fi
for file; do :; done
read -r code
echo edited >>"$file"
exit "$code"
