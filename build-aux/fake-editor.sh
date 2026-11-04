#!/bin/sh
# A stand-in for a terminal editor in the `ACCENT_BENCH_EDITOR` drills, which never run a real
# one: linked into a scratch HOME's bin as `nano`, it says what it was given, counts its starts,
# reads a line and ends with that line as its exit code, the file it was given told `edited`.
# Linked as `kak`, it ends as a program the shell cannot find does.
[ "$(basename "$0")" = kak ] && exit 127
echo "fake-editor $*"
echo $$ >>"$HOME/editor-starts"
for file; do :; done
read -r code
echo edited >>"$file"
exit "$code"
