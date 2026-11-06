#!/bin/sh
# The drills `make drills` runs, each in a headless launch of its own (build-aux/headless.sh) and
# held against what it has to print. `install_bench_hooks` (apps/gtk/src/bench/mod.rs) says what
# each `ACCENT_BENCH_*` value exercises.
#
# Usage: build-aux/drills.sh <accent binary> <libpdfium> [drill…]
# Without drill names it runs every `drill_*` function below, in order.
set -eu
accent=$1 pdfium=$2
shift 2
repo=$(cd "$(dirname "$0")/.." && pwd)
fake_lsp=$repo/build-aux/fake-lsp.py

# launch <seconds> [VAR=value…]: one run on the scratch vault.
launch() {
	secs=$1
	shift
	env "$@" timeout "$secs" "$accent" "$vault"
}

# expect <name> [<kind>]: the `bench <kind>` lines in the output file <name> are the ones in
# apps/gtk/src/bench/<name>.expected. Written to a file first, so a run that crashes or times out
# fails on its exit status as well.
expect() {
	grep "^bench ${2:+$2 }" "$dir/$1" | diff -u "$repo/apps/gtk/src/bench/$1.expected" -
}

# has <file> <line>…: each line is in the file as printed, or the file is shown.
has() {
	file=$1
	shift
	for line; do grep -qxF -- "$line" "$file" || { cat "$file"; return 1; }; done
}

# servers: the scratch vault's language servers, one `<language> = [<command>]` per line on stdin.
servers() {
	mkdir -p "$XDG_CONFIG_HOME/accent"
	{ echo "[vaults.\"$vault\".lsp.servers]"; cat; } >"$XDG_CONFIG_HOME/accent/config.toml"
}

drill_bibliography() {
	launch 90 ACCENT_BENCH_BIBLIOGRAPHY=1
}

drill_keys() {
	launch 60 ACCENT_BENCH_KEYS=1 >"$dir/keys"
	expect keys
}

drill_close() {
	echo "# Note" >"$vault/note.md"
	launch 60 ACCENT_BENCH_CLOSE=1
}

drill_goto() {
	echo "# Alpha" >"$vault/alpha.md"
	echo "# Beta" >"$vault/beta.md"
	launch 60 ACCENT_BENCH_SWITCHER=goto:alpha.md,beta.md
}

drill_toggle() {
	echo "# Alpha" >"$vault/alpha.md"
	echo "# Beta" >"$vault/beta.md"
	launch 90 ACCENT_BENCH_TABS=toggle:alpha.md,beta.md
}

drill_run_command() {
	test -e "$pdfium" || { echo "SKIPPED the Run Command drill: no libpdfium"; return 0; }
	launch 60 ACCENT_BENCH_TABS=commands
}

drill_complete() {
	echo "# Note" >"$vault/note.md"
	servers <<EOF
c = ["python3", "$fake_lsp"]
EOF
	launch 60 ACCENT_BENCH_COMPLETE=note:note.md >"$dir/complete"
	launch 60 ACCENT_BENCH_COMPLETE=code:complete.c >>"$dir/complete"
	expect complete
}

drill_quickfix() {
	servers <<EOF
c = ["python3", "$fake_lsp", "--actions"]
python3 = ["python3", "$fake_lsp", "--notebook", "--actions"]
EOF
	launch 60 ACCENT_BENCH_QUICKFIX=fix.c,fix.ipynb >"$dir/quickfix"
	expect quickfix
}

# The debugger's program runs in a terminal tab, so under a scratch HOME.
drill_debug() {
	export HOME="$dir/home"
	mkdir "$HOME"
	python3 -c "import debugpy" 2>/dev/null || { echo "SKIPPED the debugger drill: no debugpy"; return 0; }
	cat >"$vault/demo.py" <<'EOF'
def double(x):
    y = x * 2
    return y

total = double(21)
print("total", total)
EOF
	launch 60 ACCENT_BENCH_DEBUG=demo.py:2 >"$dir/debug"
	expect debug debug
}

# Under a scratch HOME too, since IPython keeps its history in `~/.ipython`.
drill_kernel() {
	export HOME="$dir/home"
	mkdir "$HOME"
	python3 -c "import ipykernel" 2>/dev/null || { echo "SKIPPED the kernel drill: no ipykernel"; return 0; }
	launch 90 ACCENT_BENCH_KERNEL=run.ipynb >"$dir/kernel"
	expect kernel kernel
}

drill_kernelspec() {
	export HOME="$dir/home" JUPYTER_PATH="$dir/jupyter"
	mkdir "$HOME"
	python3 -c "import ipykernel" 2>/dev/null || { echo "SKIPPED the kernelspec drill: no ipykernel"; return 0; }
	launch 90 ACCENT_BENCH_KERNEL=spec:spec.ipynb >"$dir/kernelspec"
	expect kernelspec kernel
}

drill_notebook_lsp() {
	servers <<EOF
python3 = ["python3", "$fake_lsp", "--notebook"]
EOF
	launch 60 ACCENT_BENCH_NOTEBOOK_LSP=lsp.ipynb,lsp_marimo.py >"$dir/notebook-lsp"
	expect notebook-lsp
}

# After the Search row, a real Ctrl+Shift+F has to take a selection on the page and one in the find
# bar's box to Search, and leave Search's query alone from that box with nothing selected.
drill_pdf_search() {
	test -e "$pdfium" || { echo "SKIPPED the PDF search drill: no libpdfium"; return 0; }
	launch 60 ACCENT_BENCH_SEARCH=pdf:zebrafish >"$dir/pdf-search"
	has "$dir/pdf-search" \
		'bench search label=pdf rows=["accent-bench-search.pdf | p. 3"]' \
		'bench search pdf opened page=3 matches=1 of 1' \
		'bench search pdf seeded "The third"' \
		'bench search find seeded "names"' \
		'bench search find kept "old query"'
}

# Its shells are real, so under a scratch HOME, running `true`, `false`, `pwd` and `sleep`.
drill_commands() {
	export HOME="$dir/home" SHELL=/bin/sh
	mkdir "$HOME"
	launch 60 ACCENT_BENCH_COMMANDS=run >"$dir/commands"
	launch 60 ACCENT_BENCH_COMMANDS=open >>"$dir/commands"
	sleep 3
	launch 60 ACCENT_BENCH_COMMANDS=back >>"$dir/commands"
	launch 60 ACCENT_BENCH_COMMANDS=prefs >>"$dir/commands"
	expect commands commands
}

# Its shells are real, so under a scratch HOME, and its editor is build-aux/fake-editor.sh alone,
# linked in as `nano` and `kak` ahead of the system's on the login shell's PATH.
drill_editor() {
	export HOME="$dir/home" SHELL=/bin/sh
	mkdir -p "$HOME/bin"
	ln -s "$repo/build-aux/fake-editor.sh" "$HOME/bin/nano"
	ln -s "$repo/build-aux/fake-editor.sh" "$HOME/bin/kak"
	# shellcheck disable=SC2016 # expanded by the login shell
	echo 'PATH="$HOME/bin:$PATH"' >"$HOME/.profile"
	launch 60 ACCENT_BENCH_EDITOR=prefs >"$dir/editor"
	launch 90 ACCENT_BENCH_EDITOR=open >>"$dir/editor"
	launch 90 ACCENT_BENCH_EDITOR=keys ACCENT_FAKE_EDITOR=keys >>"$dir/editor"
	for step in accent line panes hold back; do
		launch 60 ACCENT_BENCH_EDITOR=$step >>"$dir/editor"
	done
	fuser -k "$TMPDIR/accent-$(id -u)/hold-3.lock" >/dev/null 2>&1 || true
	sleep 1
	launch 60 ACCENT_BENCH_EDITOR=back >>"$dir/editor"
	fuser -k "$TMPDIR/accent-$(id -u)/hold-3.lock" >/dev/null 2>&1 || true
	expect editor editor
}

# Every match on screen painted, and over the word emphasis, wherever a comparison is scrolled to.
drill_find_compare() {
	launch 60 ACCENT_BENCH_FIND=compare >"$dir/find-compare"
	awk '/^bench find compare case=/ { n++; split($5, s, "="); split($6, p, "="); split($7, t, "=")
		if (s[2] == 0 || p[2] != s[2] || t[2] != s[2]) bad = 1 }
		END { exit !(n == 5 && !bad) }' "$dir/find-compare" || { cat "$dir/find-compare"; return 1; }
}

drill_preview() {
	launch 60 ACCENT_BENCH_PREVIEW=render >"$dir/preview"
	expect preview preview
}

drill_saved() {
	launch 60 ACCENT_BENCH_PREVIEW=saved >"$dir/saved"
	expect saved "preview saved"
}

# A note from a template with two stops, as a window's first tab, whose caret has to be on screen:
# a snippet pushed into a view before its first size left it past the end of a short note in about
# half the runs.
drill_template() {
	mkdir "$vault/Templates"
	cat >"$vault/Templates/Meta.md" <<'EOF'
---
accent-target: Meta.md
tags: {{cursor}}
aliases: {{cursor}}
---
EOF
	launch 60 ACCENT_BENCH_TEMPLATE=open >"$dir/template"
	has "$dir/template" "bench template_caret Meta.md on_screen=true"
}

if [ -n "${HEADLESS_DIR-}" ]; then
	# Inside the launch headless.sh set up for the one drill named.
	dir=$HEADLESS_DIR vault=$HEADLESS_DIR/vault
	"drill_$1"
else
	# shellcheck disable=SC2046 # one word per drill name
	[ $# -gt 0 ] || set -- $(sed -n 's/^drill_\([a-z_]*\)() {$/\1/p' "$0")
	for drill; do
		echo "drill $drill"
		"$repo/build-aux/headless.sh" "$0" "$accent" "$pdfium" "$drill"
	done
fi
