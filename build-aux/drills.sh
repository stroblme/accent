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

drill_opens() {
	launch 60 ACCENT_BENCH_TABS=opens >"$dir/opens"
	expect opens opens
}

# Every kind's tab up before its file arrives from a slow host, each read and fetch held 800 ms.
drill_loading() {
	test -e "$pdfium" || { echo "SKIPPED the loading drill: no libpdfium"; return 0; }
	launch 90 ACCENT_BENCH_TABS=loading >"$dir/loading"
	expect loading loading
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

# The C server publishes 50 ms late, so the fixes the hover asks for right after a line typed in
# above the error come before the server has said where the error went.
drill_quickfix() {
	servers <<EOF
c = ["python3", "$fake_lsp", "--actions", "--publish-delay", "0.05"]
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
python3 = ["python3", "$fake_lsp", "--notebook", "--saves", "$dir/saves"]
EOF
	launch 90 ACCENT_BENCH_NOTEBOOK_LSP=lsp.ipynb,lsp_marimo.py,"$dir/saves" >"$dir/notebook-lsp"
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
	# The holder has to end here, or the second `back` finds its editor still held (starts=1).
	fuser -sk "$TMPDIR/accent-$(id -u)/hold-3.lock" ||
		{ echo "the editor drill could not end the shell holder: is fuser (psmisc) installed?"; return 1; }
	sleep 1
	launch 60 ACCENT_BENCH_EDITOR=back >>"$dir/editor"
	fuser -k "$TMPDIR/accent-$(id -u)/hold-3.lock" >/dev/null 2>&1 || true
	expect editor editor
}

# Every match on screen painted, and over the word emphasis, in the column the keyboard is in and
# nowhere else, wherever a comparison or a staged change is scrolled to.
drill_find_compare() {
	launch 60 ACCENT_BENCH_FIND=compare >"$dir/find-compare"
	awk '/^bench find compare case=/ { n++; split($5, s, "="); split($6, p, "="); split($7, t, "=")
		if (s[2] == 0 || p[2] != s[2] || t[2] != s[2] || $8 != "elsewhere=0" || $9 != "readout=true") bad = 1 }
		END { exit !(n == 9 && !bad) }' "$dir/find-compare" || { cat "$dir/find-compare"; return 1; }
}

# The vault a repository: note.md committed twice, and untracked.md a row in Changes, which a
# drill waits for.
history_repo() {
	note() { awk -v at="$1" 'BEGIN { for (i = 1; i <= 80; i++) print (i % at ? "Line " i : "Changed " i) ".\n" }'; }
	commit() { git -C "$vault" -c user.name=drill -c user.email=drill@invalid -c commit.gpgsign=false commit -qam "$1"; }
	git -C "$vault" init -q
	note 1000 >"$vault/note.md"
	git -C "$vault" add note.md
	commit one
	note 30 >"$vault/note.md"
	commit two
	echo "# Untracked" >"$vault/untracked.md"
}

# A file from the history, Show Changes let go, shows the commit's version alone from the line the
# comparison had at its top, and the comparison again once it is pressed, and the palette's Show
# Changes lets it go again.
drill_history_file() {
	history_repo
	launch 60 ACCENT_BENCH_COMPARE=row:commit:note.md >"$dir/history-file"
	grep -Eqx 'bench compare_row alone offered=true shown=true text=true top=([0-9]+)->\1' "$dir/history-file" ||
		{ cat "$dir/history-file"; return 1; }
	has "$dir/history-file" 'bench compare_row alone pressed shown=false down=true' \
		'bench compare_row alone action listed=true ran=true shown=true state=Some(false)'
}

# On a slow host, each read held 800 ms, a Git row's tab is up before git answers and shows the
# loading page: one of its own for a file from the history, which opens with its unchanged runs
# hidden as a quick one does, and the file's own for a change in the working tree. The first read
# of each fails, which the tab says with Retry and no toast, Retry comparing again. The commit's
# row goes first, the second launch restoring its tab on the loading page.
drill_compare_loading() {
	history_repo
	export ACCENT_BENCH_SLOW_READ=800 ACCENT_BENCH_FAIL_GIT_READ=1
	launch 60 ACCENT_BENCH_COMPARE=row:commit:note.md >"$dir/compare-loading"
	launch 60 ACCENT_BENCH_COMPARE=row:untracked.md >>"$dir/compare-loading"
	has "$dir/compare-loading" 'bench compare_row at_once file=None diff=Some("diff/loading")' \
		'bench compare_row failed file=None diff=Some("diff/failed") retry=true toasts=0' \
		'bench compare_row restored Some("diff/loading")' \
		'bench compare_row at_once file=Some("loading/loading") diff=None' \
		'bench compare_row failed file=Some("loading/failed") diff=None retry=true toasts=0'
	grep -Eq '^bench compare_row opened key=.* hidden=[1-9]' "$dir/compare-loading" &&
		grep -q '^bench compare_row opened rel="untracked.md"' "$dir/compare-loading" ||
		{ cat "$dir/compare-loading"; return 1; }
}

drill_preview() {
	launch 60 ACCENT_BENCH_PREVIEW=render >"$dir/preview"
	expect preview preview
}

drill_saved() {
	launch 60 ACCENT_BENCH_PREVIEW=saved >"$dir/saved"
	expect saved "preview saved"
}

# A notebook's and a diagram's saves and reloads, a fresh tab a case. An answer to the conflict
# dialog holds the main loop for no write, so its `held_ms` stays under a second and is left out
# of what is compared.
drill_answer_doc() {
	launch 180 ACCENT_BENCH_ANSWER=doc:answer.ipynb,answer.drawio >"$dir/answer-doc.out"
	sed -n 's/.* held_ms=\([0-9]*\).*/\1/p' "$dir/answer-doc.out" | awk '$1 >= 1000 { bad = 1 } END { exit bad }' ||
		{ cat "$dir/answer-doc.out"; return 1; }
	sed 's/ held_ms=[0-9]*//' "$dir/answer-doc.out" >"$dir/answer-doc"
	expect answer-doc "answer doc"
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

# A command for each template that names a target, picked, given a chord and pressed, then gone
# with its template while its chord stays stored. Loose names none, so it has no command.
drill_template_commands() {
	mkdir "$vault/Templates"
	cat >"$vault/Templates/Meta.md" <<'EOF'
---
accent-target: Meta.md
---
# Meta
EOF
	cat >"$vault/Templates/Inbox Note.md" <<'EOF'
---
accent-target: Inbox/{{title}}.md
---
# {{title}}
EOF
	echo "# Loose" >"$vault/Templates/Loose.md"
	launch 60 ACCENT_BENCH_TEMPLATE=commands >"$dir/template-commands"
	has "$dir/template-commands" \
		'bench template_commands rows=[("New Inbox Note", "win.new-template-Templates-2fInbox-20Note.md"), ("New Meta", "win.new-template-Templates-2fMeta.md")]' \
		'bench template_commands run made=true used=Some("win.new-template-Templates-2fMeta.md")' \
		'bench template_commands chord opened=true made=true' \
		'bench template_commands deleted gone=true rows=[("New Meta", "win.new-template-Templates-2fMeta.md")] stored=true front=Some("Meta.md")' \
		'bench template_commands restored accels=Some([])'
}

# An open note renamed to other extensions: the same tab while it stays the same kind of page, its
# place in the bar otherwise.
drill_retype() {
	printf '# Retype\n\nSome text.\n' >"$vault/retype.md"
	echo "# Other" >"$vault/other.md"
	launch 60 ACCENT_BENCH_RETYPE=1 >"$dir/retype"
	has "$dir/retype" \
		'bench retype retype.org doc=text flavour=Some(Note(Org)) language=Some("org") same=true undo=true at=Some(0)' \
		'bench retype retype.txt doc=text flavour=Some(Code) language=Some("text") same=false undo=false at=Some(0)' \
		'bench retype retype.py doc=text flavour=Some(Code) language=Some("python3") same=true undo=true at=Some(0)' \
		'bench retype retype.png doc=image flavour=None language=None same=false undo=false at=Some(0)'
}

# A diagram's pages turned by real wheel notches: pulled off their edge while edited, turned at the
# third notch, a spin turning a page every third, and a presented one turned by a notch (DESIGN.md,
# Diagram).
drill_diagram_spring() {
	launch 60 ACCENT_BENCH_DIAGRAM_NAV=spring:spring.drawio >"$dir/spring"
	has "$dir/spring" \
		'bench nav spring one notch page=1 pull=21 rested=0' \
		'bench nav spring two notches page=1 pull=43 rested=0' \
		'bench nav spring three notches page=2 pull=43 rested=0' \
		'bench nav spring a spin of six page=4 pull=43 rested=0' \
		'bench nav spring one notch up page=4 pull=-21 rested=0' \
		'bench nav spring presented one notch page=5 pull=0'
}

# A steady wheel reading on through a diagram's pages: fitted ones edited a page every third notch,
# at 120 ms a notch and at 450 ms, the page springing back between, presented ones a page a notch,
# pages taller than the view scrolled through and turned at each edge, down and up, and Alt paging
# them a notch a page onto each one's top at the zoom it was at (DESIGN.md, Diagram).
drill_diagram_steady() {
	launch 90 ACCENT_BENCH_DIAGRAM_NAV=steady:steady.drawio >"$dir/steady"
	has "$dir/steady" \
		'bench nav steady fitted nine at 120 ms pages=1>2>3>4 pull=0' \
		'bench nav steady fitted nine at 450 ms pages=1>2>3>4 pull=0' \
		'bench nav steady presented four at 120 ms pages=1>2>3>4>5 pull=0' \
		'bench nav steady tall down twenty-eight at 120 ms pages=1>2>3 pull=0' \
		'bench nav steady tall up twenty-eight at 120 ms pages=4>3>2 pull=0' \
		'bench nav steady tall alt down three at 120 ms pages=1>2>3>4 pull=0' \
		'bench nav steady alt down page=4 zoom=1.00 on_top=true' \
		'bench nav steady tall alt up three at 120 ms pages=4>3>2>1 pull=0' \
		'bench nav steady alt up page=1 zoom=1.00 on_top=true'
}

# A diagram's labels found in the find bar through its pages and round, by a regular expression
# and not by one that does not compile, and a page gone to from Go to Diagram Page… (DESIGN.md,
# Find bar).
drill_diagram_find() {
	launch 60 ACCENT_BENCH_DIAGRAM_NAV=find:find.drawio >"$dir/find"
	has "$dir/find" \
		'bench nav find alpha readout="1 of 2" page=1 selected=["a"]' \
		'bench nav find next readout="2 of 2" page=2 selected=["c"]' \
		'bench nav find round readout="1 of 2" page=1 selected=["a"]' \
		'bench nav find regex readout="1 of 1" page=1 selected=["b"]' \
		'bench nav find invalid readout="Invalid pattern" page=1 selected=["b"]' \
		'bench nav find go to page 3 readout="" page=3 selected=[]'
}

# A diagram's links shown with Ctrl held and followed by a click on that popover or on a presented
# page, Ctrl+click and a plain click while editing selecting, and the popover of a link near each
# corner of the pane inside it, below a link at the top and above one at the foot, as is one
# whose thumbnail of a page taller than the pane is cut to the room and centred above its wider
# caption (DESIGN.md, Diagram).
drill_diagram_links() {
	launch 60 ACCENT_BENCH_DIAGRAM_NAV=links:links.drawio >"$dir/links"
	has "$dir/links" \
		'bench nav link preview=Some("https://example.org/")' \
		'bench nav link ctrl-click page=1 selected=["go"]' \
		'bench nav link popover page=2 selected=[]' \
		'bench nav link click page=2 selected=["back"]' \
		'bench nav link presented click page=1 selected=[]' \
		'bench nav link corner top-left side=Bottom inside=true' \
		'bench nav link corner top-right side=Bottom inside=true' \
		'bench nav link corner bottom-left side=Top inside=true' \
		'bench nav link corner bottom-right side=Top inside=true' \
		'bench nav link tall inside=true' \
		'bench nav link tall centred=true'
}

# A diagram's picture named by a path, read beside it in the vault, on the canvas and in a note's
# embed, with no web banner.
drill_diagram_file_picture() {
	mkdir -p "$vault/d/pics"
	echo iVBORw0KGgoAAAANSUhEUgAAAAQAAAAECAIAAAAmkwkpAAAAEElEQVR4nGP4z8AARwzEcQCukw/x0F8jngAAAABJRU5ErkJggg== |
		base64 -d >"$vault/d/pics/red.png"
	launch 60 ACCENT_BENCH_DIAGRAM=web:d/pic.drawio:pics/red.png >"$dir/file_picture"
	has "$dir/file_picture" \
		'bench diagram web opened banner=false allowed=false pixel=Some([255, 0, 0]) downloaded=false' \
		'bench diagram web embed pictures=1'
}

# What real clicks and bands select among a diagram's groups, as draw.io selects (DESIGN.md,
# Diagram).
drill_diagram_select() {
	launch 60 ACCENT_BENCH_DIAGRAM=select:select.drawio >"$dir/select"
	has "$dir/select" \
		'bench diagram select band over g ["g", "c", "d"]' \
		'bench diagram select band over c1 ["c1"]' \
		'bench diagram select click c ["g"]' \
		'bench diagram select click c again ["c"]' \
		'bench diagram select click sibling d ["d"]' \
		'bench diagram select click c1 ["G"]' \
		'bench diagram select click c1 again ["g1"]' \
		'bench diagram select click c1 a third time ["c1"]' \
		'bench diagram select click cousin d1 ["g2"]' \
		'bench diagram select click c again moving 2 px ["c"]' \
		'bench diagram select click inside g between its shapes ["c", "d"]' \
		'bench diagram select band from inside g over c2 and d1 ["c2", "d1"]' \
		'bench diagram select first click p1 ["p"]' \
		'bench diagram select first click t1 ["t1"]' \
		'bench diagram select first click xl ["xl"]' \
		'bench diagram select open inside g Some("g")' \
		'bench diagram select paste inside g [Some("g")] open=Some("g")' \
		'bench diagram select paste outside [Some("1")] open=None'
}

# Ctrl+V with a browser's HTML on the clipboard, and Paste as Plain Text, in a note (beside its code
# block, table and front matter too), an org note, a code tab, a note renamed between markdown and
# org, a notebook's cells and a diagram.
drill_paste() {
	printf -- '---\ntitle: Note\n---\n\n# Note\n\nbefore\n\n```text\ncode\n```\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\nend\n' >"$vault/paste.md"
	echo "* Org" >"$vault/paste.org"
	echo "x = 1" >"$vault/paste.py"
	cat >"$vault/paste.ipynb" <<'EOF'
{"cells": [{"cell_type": "markdown", "metadata": {}, "source": ["Notes\n"]},
 {"cell_type": "code", "execution_count": null, "metadata": {}, "outputs": [], "source": ["x = 1\n"]}],
 "metadata": {}, "nbformat": 4, "nbformat_minor": 5}
EOF
	cat >"$vault/paste.drawio" <<'EOF'
<mxfile><diagram id="p" name="Page-1"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/></root></mxGraphModel></diagram></mxfile>
EOF
	launch 60 ACCENT_BENCH_PASTE=1 >"$dir/paste"
	expect paste paste
}

# The vault's root in each sort, with a folder left open staying open. Each file's creation time
# is the order it was written in, c, a then B, apart by more than a coarse clock's tick.
drill_sort() {
	mkdir "$vault/Alpha" "$vault/beta"
	for f in c a B beta/inner; do
		echo "# $f" >"$vault/$f.md"
		sleep 0.1
	done
	touch -m -d @3000 "$vault/a.md"
	touch -m -d @1000 "$vault/B.md"
	touch -m -d @2000 "$vault/c.md"
	touch -m -d @500 "$vault/Alpha"
	touch -m -d @4000 "$vault/beta"
	launch 60 ACCENT_BENCH_SORT=1 >"$dir/sort"
	has "$dir/sort" \
		'bench sort name-asc ["Alpha", "beta", "a.md", "B.md", "c.md"] open=true' \
		'bench sort name-desc ["beta", "Alpha", "c.md", "B.md", "a.md"] open=true' \
		'bench sort modified-new ["Alpha", "beta", "a.md", "c.md", "B.md"] open=true' \
		'bench sort modified-old ["Alpha", "beta", "B.md", "c.md", "a.md"] open=true' \
		'bench sort created-new ["Alpha", "beta", "B.md", "a.md", "c.md"] open=true' \
		'bench sort created-old ["Alpha", "beta", "c.md", "a.md", "B.md"] open=true' \
		'bench sort among ["beta", "a.md", "c.md", "B.md", "Alpha"] open=true' \
		'bench sort session modified-new'
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
