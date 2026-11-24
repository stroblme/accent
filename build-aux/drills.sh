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

# A note's picture written from outside after the note opened takes the place of its missing sign.
drill_picture_arrives() {
	printf '![a](a.png)\n' >"$vault/note.md"
	cp "$repo/docs/guide/social-preview.png" "$vault/b.png"
	launch 60 ACCENT_BENCH_PREVIEW_LOOK=change:note.md,a.png,b.png >"$dir/arrives"
	if ! grep -q '^bench preview_look before .*/a.png loaded=false ' "$dir/arrives" ||
		! grep -q '^bench preview_look changed .*/a.png loaded=true ' "$dir/arrives"; then
		cat "$dir/arrives"
		return 1
	fi
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

# A notebook's cells debugged in its kernel, under a scratch HOME as the kernel's drill is.
drill_notebook_debug() {
	export HOME="$dir/home"
	mkdir "$HOME"
	python3 -c "import ipykernel, debugpy" 2>/dev/null ||
		{ echo "SKIPPED the notebook debugger drill: no ipykernel or debugpy"; return 0; }
	launch 120 ACCENT_BENCH_NOTEBOOK_DEBUG=nbdebug.ipynb >"$dir/notebook-debug"
	expect notebook-debug nbdebug
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

# A drawing ring in the pane that is not the active one takes a real press, beside a PDF and beside
# a diagram, and picks the tool on its own tab alone against what that ring shows, and every pane's
# ring is out again after Reload Window (DESIGN.md, PDF).
drill_ring_panes() {
	test -e "$pdfium" || { echo "SKIPPED the ring panes drill: no libpdfium"; return 0; }
	launch 90 ACCENT_BENCH_PDF=ring-panes >"$dir/ring-panes"
	has "$dir/ring-panes" \
		'bench ring panes pdfs greyed=0 of=18' \
		"bench ring panes pdfs window's Line pressed=true a=Line b=Line active=true keyboard=true" \
		'bench ring panes pdfs shown Line pressed=true a=Line b=Read active=true keyboard=true' \
		'bench ring panes pdfs other Circle pressed=true a=Line b=Circle active=true keyboard=true' \
		'bench ring panes diagram held greyed=0 of=16' \
		'bench ring panes diagram Delete Selection pressed=true a=Line deleted=true active=true keyboard=true' \
		'bench ring panes diagram greyed=0 of=8' \
		'bench ring panes diagram Add Rectangle pressed=true a=Line tool=Rect active=true keyboard=true' \
		'bench ring panes beside diagram greyed=0 of=18' \
		'bench ring panes beside diagram Highlighter pressed=true a=Highlighter tool=Rect active=true keyboard=true' \
		'bench ring panes split ring-a.pdf*=true ring-b.pdf=true ring.drawio=true' \
		'bench ring panes restored ring-a.pdf*=true ring-b.pdf=true ring.drawio=true'
}

# A finger's selection drag shows the loupe above the line under it, kept to that line however the
# finger wobbles, sharp once its band has landed and on the screen at the view's top and its left
# edge, a pen's too and a mouse's not, through the view's own path and through XTEST. The handles
# it leaves move either end out and back to where it was, under a finger and an XTEST mouse, and
# a mouse's selection, a key and the keyboard leaving put them away. An XTEST mouse's drag held at
# the view's bottom edge scrolls on, its selection running from the text pressed to the second
# page (DESIGN.md, PDF).
drill_pdf_loupe() {
	test -e "$pdfium" || { echo "SKIPPED the PDF loupe drill: no libpdfium"; return 0; }
	launch 150 ACCENT_BENCH_PDF=loupe >"$dir/loupe"
	for want in \
		'read shown=true ' \
		'touch step=5 shown=true .* above=true .* on_screen=true .* selected="The loupe' \
		'sharp shown=true .* source_scale=2 ' \
		'lifted none selected="The loupe.* handles=start=(' \
		'grip end took=true' \
		'grip end step=1 dx=60 shown=true .* source_scale=2 ' \
		'grip start took=true' \
		'grip start step=1 dx=60 shown=true ' \
		'xtest_grip grown=true' \
		'heading shown=true .* above=false .* on_screen=true ' \
		'edge shown=true .* on_screen=true ' \
		'pen shown=true .* handles=start=(' \
		'key gone=true' \
		'focus_out up=true gone=true' \
		'mouse none .* handles=none' \
		'xtest_mouse none selected="The loupe.* handles=none' \
		'xtest_mouse changed=true' \
		'xtest_edge reached=true kept=true' \
		'dense shown=true .* source_scale=2 '; do
		grep -q "^bench loupe $want" "$dir/loupe" || { cat "$dir/loupe"; return 1; }
	done
	lines=$(sed -n 's/^bench loupe touch .* source=([^,]*,\([^,]*\),.*/\1/p' "$dir/loupe" | sort -u | wc -l)
	test "$lines" -eq 1 || { cat "$dir/loupe"; return 1; }
	# Out, the end's handle grows the selection past where it ended and the start's takes it in
	# from where it began, the other end staying; back, each leaves it as it found it.
	kept=$(sed -n 's/^bench loupe lifted .* selected="\(.*\)" handles=.*/\1/p' "$dir/loupe")
	sel() { sed -n "s/^bench loupe grip $1 step=$2 .* selected=\"\(.*\)\" handles=.*/\1/p" "$dir/loupe"; }
	grown=$(sel end 1) shrunk=$(sel start 1)
	case $grown in "$kept"?*) ;; *) cat "$dir/loupe"; return 1 ;; esac
	case $kept in ?*"$shrunk") test -n "$shrunk" ;; *) false ;; esac || { cat "$dir/loupe"; return 1; }
	test "$(sel end 3)" = "$kept" && test "$(sel start 3)" = "$kept" || { cat "$dir/loupe"; return 1; }
}

# The fixture form's fields as the tab reads them, the pointer and the tooltip over each, its check
# box and radio buttons clicked and undone, a text field typed into at 100 % and 200 %, left by
# Escape and undone, a combo box's and a list box's options picked from their lists, Tab through
# every field and Space on one, and the form reset from the palette and by its button, each
# undone, against the file (DESIGN.md, PDF Forms).
drill_pdf_form() {
	test -e "$pdfium" || { echo "SKIPPED the PDF form drill: no libpdfium"; return 0; }
	launch 90 ACCENT_BENCH_PDF=form >"$dir/form"
	has "$dir/form" \
		'bench form fields page=0 count=13 name:text address:multiline person.email:text agree:check size:radio size:radio code:comb amount:text(read-only) country:combo fruit:list secret:password reset:button sign:signature' \
		'bench form cursor name=text address=text person.email=text agree=pointer size=pointer size=pointer code=text amount=default country=pointer fruit=pointer secret=default reset=pointer sign=default' \
		'bench form tips name="Your name" secret="Accent does not store passwords"' \
		'bench form ticked name="Ada" agree=true size=false size=true amount="12.50" country=[fr] fruit=[Apple,Cherry]' \
		'bench form picked name="Ada" agree=true size=true size=false amount="12.50" country=[fr] fruit=[Apple,Cherry]' \
		'bench form undone name="Ada" agree=true size=false size=true amount="12.50" country=[fr] fruit=[Apple,Cherry]' \
		'bench form editing open over=Some(true) size=12.0 focus=true' \
		'bench form typed closed name="Ada Lovelace" agree=true size=false size=true amount="12.50" country=[fr] fruit=[Apple,Cherry]' \
		'bench form zoomed open over=Some(true) size=12.0 focus=true' \
		'bench form escaped closed name="Ada Lovelace" agree=true size=false size=true amount="12.50" country=[fr] fruit=[Apple,Cherry]' \
		'bench form typed_undone name="Ada" agree=true size=false size=true amount="12.50" country=[fr] fruit=[Apple,Cherry]' \
		'bench form choosing open=true rows=3' \
		'bench form chose open=false name="Ada" agree=true size=false size=true amount="12.50" country=[it] fruit=[Apple,Cherry]' \
		'bench form ticked_list name="Ada" agree=true size=false size=true amount="12.50" country=[it] fruit=[Apple,Banana,Cherry]' \
		'bench form list_undone name="Ada" agree=true size=false size=true amount="12.50" country=[it] fruit=[Apple,Cherry]' \
		'bench form spaced name="Ada" agree=false size=false size=true amount="12.50" country=[it] fruit=[Apple,Cherry]' \
		'bench form tabbed name person.email address agree size size code country fruit reset address size -' \
		'bench form reset agree=false size=false size=false country=[] fruit=[]' \
		'bench form reset_undone name="Ada" agree=false size=false size=true amount="12.50" country=[it] fruit=[Apple,Cherry]' \
		'bench form pressed agree=false size=false size=false country=[] fruit=[]' \
		'bench form pressed_undone name="Ada" agree=false size=false size=true amount="12.50" country=[it] fruit=[Apple,Cherry]' \
		'bench form returned closed'
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
	for step in accent line panes tasks hold back; do
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

# A shell's paths lit under the pointer and opened by Ctrl+click through XTEST. The shell is real,
# so under a scratch HOME, and `sh` reports no folder (no OSC 7), so the holder is asked for it.
drill_terminal_links() {
	export HOME="$dir/home" SHELL=/bin/sh
	mkdir "$HOME"
	launch 60 ACCENT_BENCH_LINKS=1 >"$dir/links"
	fuser -k "$TMPDIR/accent-$(id -u)/hold-3.lock" >/dev/null 2>&1 || true
	expect links links
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

# The Git pane on a slow link, each git call held 40 ms: a Stage, an Unstage, a Commit, a Sync and
# a branch switch hold the pane until the refresh after them shows what they did, a Stage pressed
# while an older refresh is on its way included, and a Commit pressed the moment a Stage lets go
# commits what was staged. A pick of a repository never picked shows its history's bar until its
# rows come. The vault a repository of 210 commits, one behind its upstream, with a
# branch `other`, and a second repository in it, for a pick.
drill_git_refresh() {
	for r in "$vault" "$vault/inner"; do
		git init -q -b main "$r"
		git -C "$r" config user.name drill
		git -C "$r" config user.email drill@invalid
		git -C "$r" config commit.gpgsign false
	done
	for f in note other; do echo "# $f" >"$vault/$f.md"; echo "# $f" >"$vault/inner/$f.md"; done
	git -C "$vault/inner" add -A
	git -C "$vault/inner" commit -qm one
	git -C "$vault" add note.md other.md
	git -C "$vault" commit -qm one
	for i in $(seq 2 210); do echo "$i" >>"$vault/other.md"; git -C "$vault" commit -qam "$i"; done
	git -C "$vault" branch other HEAD~2
	git clone -q --bare "$vault" "$dir/origin.git"
	git -C "$vault" remote add origin "$dir/origin.git"
	git -C "$vault" fetch -q origin
	git -C "$vault" branch -q -u origin/main
	git clone -q "$dir/origin.git" "$dir/peer"
	echo upstream >"$dir/peer/up.md"
	git -C "$dir/peer" add up.md
	git -C "$dir/peer" -c user.name=drill -c user.email=drill@invalid -c commit.gpgsign=false commit -qm up
	git -C "$dir/peer" push -q origin main
	launch 120 ACCENT_BENCH_SLOW_GIT=40 ACCENT_BENCH_GIT=refresh:note.md,other.md >"$dir/git-refresh"
	if grep -Eq '^bench git_refresh .*(ms=timeout|stale=[1-9]|bare=[1-9])' "$dir/git-refresh" ||
		[ "$(grep -c '^bench git_refresh .* stale=0 bare=0$' "$dir/git-refresh")" -ne 18 ]; then
		cat "$dir/git-refresh"
		return 1
	fi
	has "$dir/git-refresh" 'bench git_refresh stage_commit committed=note.md'
}

# A history whose first `git log` fails, as one over a link that drops does, says so in its place
# with Retry and no toast, and Retry brings its rows.
drill_git_log_failed() {
	history_repo
	launch 60 ACCENT_BENCH_FAIL_GIT_LOG=1 ACCENT_BENCH_GIT=log_failed >"$dir/git-log-failed"
	has "$dir/git-log-failed" \
		'bench git_log_failed why="the host did not answer." rows=0 toasted=false' \
		'bench git_log_failed retried failure=None rows=true'
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

# Each kind of task box clicked in a preview through XTEST: beside its note, alone, presented.
drill_tasks() {
	launch 90 ACCENT_BENCH_PREVIEW=tasks >"$dir/tasks"
	expect tasks tasks
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
		'bench template_commands run made=true used=None' \
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
# embed, with no web banner, and drawn again in both as its file changes, goes and comes back.
drill_diagram_file_picture() {
	mkdir -p "$vault/d/pics"
	echo iVBORw0KGgoAAAANSUhEUgAAAAQAAAAECAIAAAAmkwkpAAAAEElEQVR4nGP4z8AARwzEcQCukw/x0F8jngAAAABJRU5ErkJggg== |
		base64 -d >"$vault/d/pics/red.png"
	launch 60 ACCENT_BENCH_DIAGRAM=web:d/pic.drawio:pics/red.png >"$dir/file_picture"
	has "$dir/file_picture" \
		'bench diagram web opened banner=false allowed=false pixel=Some([255, 0, 0]) downloaded=false' \
		'bench diagram web embed pictures=1' \
		'bench diagram web changed banner=false allowed=false pixel=Some([0, 0, 255]) downloaded=false' \
		'bench diagram web changed embed pictures=1 new=true' \
		'bench diagram web removed banner=false allowed=false pixel=Some([255, 255, 255]) downloaded=false' \
		'bench diagram web removed embed pictures=0 new=true' \
		'bench diagram web added banner=false allowed=false pixel=Some([255, 0, 0]) downloaded=false' \
		'bench diagram web added embed pictures=1 new=true'
}

# The same of a diagram outside the vault, whose picture's folder no vault's watcher sees.
drill_diagram_loose_picture() {
	mkdir -p "$dir/loose/pics"
	echo iVBORw0KGgoAAAANSUhEUgAAAAQAAAAECAIAAAAmkwkpAAAAEElEQVR4nGP4z8AARwzEcQCukw/x0F8jngAAAABJRU5ErkJggg== |
		base64 -d >"$dir/loose/pics/red.png"
	launch 60 "ACCENT_BENCH_DIAGRAM=web:$dir/loose/pic.drawio:pics/red.png" >"$dir/loose_picture"
	has "$dir/loose_picture" \
		'bench diagram web opened banner=false allowed=false pixel=Some([255, 0, 0]) downloaded=false' \
		'bench diagram web embed pictures=1' \
		'bench diagram web changed banner=false allowed=false pixel=Some([0, 0, 255]) downloaded=false' \
		'bench diagram web changed embed pictures=1 new=true' \
		'bench diagram web removed banner=false allowed=false pixel=Some([255, 255, 255]) downloaded=false' \
		'bench diagram web removed embed pictures=0 new=true' \
		'bench diagram web added banner=false allowed=false pixel=Some([255, 0, 0]) downloaded=false' \
		'bench diagram web added embed pictures=1 new=true'
}

# A page `accent --render-diagram` shrinks to a pixel a unit, its 1 wide line on whole pixels of
# that and a line 0 wide drawn a tenth of a pixel wide, as draw.io draws them (DESIGN.md, Diagram).
drill_diagram_render() {
	launch 60 ACCENT_BENCH_DIAGRAM=render >"$dir/render"
	has "$dir/render" 'bench diagram render size=102x1568 line=[255, 0, 255] hairline=true'
}

# A picture in a diagram's PDF as pdfium draws it at 72, 99, 150 and 300 dpi, with no seam around
# its box darker than the paper.
drill_diagram_edges() {
	test -e "$pdfium" || { echo "SKIPPED the picture edges drill: no libpdfium"; return 0; }
	launch 60 ACCENT_BENCH_DIAGRAM=edges >"$dir/edges"
	has "$dir/edges" 'bench diagram edges drawn=true seams=[0, 0, 0, 0]'
}

# A diagram's PDF as pdfium reads its outline: a bookmark for each page, in order, named as the
# Outline lists it, an unnamed one "Page 2".
drill_diagram_outline() {
	test -e "$pdfium" || { echo "SKIPPED the diagram outline drill: no libpdfium"; return 0; }
	launch 60 ACCENT_BENCH_DIAGRAM=outline >"$dir/outline"
	has "$dir/outline" 'bench diagram outline [(0, "Grüße", Some(0)), (0, "Page 2", Some(1)), (0, "Plan (v2) \\ draft", Some(2)), (0, "图表 ∑ 🚀", Some(3))]'
}

# What real clicks and bands select among a diagram's groups, as draw.io selects, a drag moving
# only what is selected, and how the groups fit what they hold (DESIGN.md, Diagram).
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
		'bench diagram select click t1 again ["tb"]' \
		'bench diagram select tb frame (300.0, 280.0, 200.0, 60.0)' \
		'bench diagram select click t1 a third time ["t1"]' \
		'bench diagram select band from lane p over p1 ["p1"]' \
		'bench diagram select band from p moved=false history=(false, false)' \
		'bench diagram select ctrl band from lane p over p1 ["s1", "p1"]' \
		'bench diagram select click s2 moving 4 px []' \
		'bench diagram select click s2 moving 2 px ["s2"]' \
		'bench diagram select cursor s2=Some("move") p1=None' \
		'bench diagram select drag selected s2 unmoved_before=true s2=Some((600.0, 160.0)) history=(true, false)' \
		'bench diagram select drag c of selected g ["g"]' \
		'bench diagram select drag c of selected g g=Some((40.0, 80.0))' \
		'bench diagram select undone true' \
		'bench diagram select select all ["g", "c", "d", "G", "g1", "c1", "c2", "g2", "d1", "d2", "p", "p1", "tb", "t1", "t2", "s1", "s2", "x", "xl"]' \
		'bench diagram select open inside g Some("g")' \
		'bench diagram select paste inside g [Some("g")] open=Some("g")' \
		'bench diagram select paste outside [Some("1")] open=None' \
		'bench diagram select fit move c g=Some("0,40,240,100") c=Some("0,40,80,60") d=Some("160,0,80,60")' \
		'bench diagram select fit delete d g=Some("0,50,130,90") c=Some("0,30,80,60") d=None' \
		'bench diagram select fit delete the rest g=None c=None d=None' \
		'bench diagram select fit move tb tb=Some("0,0,0,0") t1=Some("320,280,80,60")' \
		'bench diagram select fit resize G G=Some("40,160,660,90") g2=Some("360,0,300,90") d2=Some("180,0,120,90")'
}

# An arrow's waypoints added from the canvas's menu, taken away by a double click on their handle and
# by the menu's Remove Waypoint, and cleared from a shape the arrow is on, each one undo step; a
# resize with Shift held keeping the box's proportions; a band held on the fitted sheet's corner
# scrolling nothing; and a move held at the canvas's bottom edge scrolling on past where the canvas
# ended and landing there (DESIGN.md, Diagram).
drill_diagram_handles() {
	launch 60 ACCENT_BENCH_DIAGRAM=handles:handles.drawio >"$dir/handles"
	has "$dir/handles" \
		'bench diagram handles menu line ["Add Waypoint", "Clear Waypoints"] picked=true' \
		'bench diagram handles added points=[(250.0, 130.0)] history=(true, false)' \
		'bench diagram handles double clicked points=[] history=(true, false)' \
		'bench diagram handles undone points=[(250.0, 130.0)] history=(true, true)' \
		'bench diagram handles menu waypoint ["Remove Waypoint", "Clear Waypoints"] picked=true' \
		'bench diagram handles removed points=[] history=(true, false)' \
		'bench diagram handles menu shape ["Clear Waypoints"] picked=true' \
		'bench diagram handles cleared points=[] history=(true, false)' \
		'bench diagram handles shift resize s=Some("100,300,120,90")' \
		'bench diagram handles corner scrolled=false' \
		'bench diagram handles edge beyond_canvas=true under_pointer=true'
}

# The Shape tool through real input: its ring button and slot, the grid the slot opens on the shape
# picked, the arrow keys moving through it and on from one palette's grid to the next, Escape
# keeping the pick and Return taking another, a drag drawing it in its palette entry's style, a
# Basic shape clicked in the grid, and a click dropping it at its entry's size (DESIGN.md, Diagram).
drill_shape_tool() {
	launch 60 ACCENT_BENCH_DIAGRAM=shape:shape.drawio >"$dir/shape"
	has "$dir/shape" \
		'bench shape ring tool=Shape slot=Diamond checked=true' \
		'bench shape grid open=true general=20 selected=["Diamond"] focus=Some("Diamond")' \
		'bench shape right focus=Some("Parallelogram") selected=["Parallelogram"]' \
		'bench shape escape open=false slot=Diamond config=Diamond tool=Shape keyboard=true' \
		'bench shape return open=false slot=Triangle config=Triangle tool=Shape keyboard=true' \
		'bench shape drawn title=Triangle size=80x80 match=true style=triangle;whiteSpace=wrap;html=1;shapeInside=1; tool=Select' \
		'bench shape click open=false slot=Diagonal Rounded Rectangle config=Diagonal Rounded Rectangle tool=Shape keyboard=true' \
		'bench shape Home palette=Basic focus=Partial Rectangle' \
		'bench shape Up palette=General focus=And' \
		'bench shape Down palette=Basic focus=Partial Rectangle' \
		'bench shape dropped title=Diagonal Rounded Rectangle size=100x60 match=true style=verticalLabelPosition=bottom;verticalAlign=top;html=1;shape=mxgraph.basic.diag_round_rect;dx=6;whiteSpace=wrap; tool=Select'
	# How many Basic shapes show changes as more are ported.
	grep -q '^bench shape basic shown=[0-9]* diagonal=true$' "$dir/shape" || { cat "$dir/shape"; return 1; }
}

# The style editor: Edit Style's tab of a cell's style, what is typed reaching the cell after the
# pause, the diagram's Undo showing in the tab, completion, the star's state, Save and the session
# leaving no trace of it, and the tab closing with its cell and with its diagram (DESIGN.md,
# Diagram).
drill_diagram_style() {
	launch 60 ACCENT_BENCH_DIAGRAM=style:style.drawio >"$dir/style"
	has "$dir/style" \
		'bench diagram style star Some(("non-starred-symbolic", "Set Default Style for rectangles"))' \
		'bench diagram style opened title="A (style.drawio)" front=true language=drawio-style text="rounded=1\nwhiteSpace=wrap\nhtml=1\nfillColor=#dae8fc\nstrokeColor=#6c8ebf"' \
		'bench diagram style complete "fil" rows=["fillColor", "fillOpacity"]' \
		'bench diagram style complete "shape=ell" rows=["ellipse", "doubleEllipse"]' \
		'bench diagram style untouched a=Some("rounded=1;whiteSpace=wrap;html=1;fillColor=#dae8fc;strokeColor=#6c8ebf;") history=(false, false)' \
		'bench diagram style typing a=Some("rounded=1;whiteSpace=wrap;html=1;fillColor=#dae8fc;strokeColor=#6c8ebf;")' \
		'bench diagram style applied a=Some("rounded=1;whiteSpace=wrap;html=1;fillColor=#dae8fc;strokeColor=#6c8ebf;shadow=1;") history=(true, false)' \
		'bench diagram style undone text="rounded=1\nwhiteSpace=wrap\nhtml=1\nfillColor=#dae8fc\nstrokeColor=#6c8ebf"' \
		'bench diagram style again editors=1 front=true' \
		'bench diagram style set Some(("starred-symbolic", "Clear Default Style for rectangles"))' \
		'bench diagram style cleared Some(("non-starred-symbolic", "Set Default Style for rectangles"))' \
		'bench diagram style undo_clear Some(("starred-symbolic", "Clear Default Style for rectangles"))' \
		'bench diagram style deleted editors=0' \
		'bench diagram style typed "shape=" rows=["label", "rectangle"]' \
		'bench diagram style saved a=Some("rounded=1;whiteSpace=wrap;html=1;fillColor=#dae8fc;strokeColor=#6c8ebf;shadow=1;") files=0 session=false' \
		'bench diagram style closed editors=0 written=true'
}

# Ctrl+V with a browser's HTML on the clipboard, and Paste as Plain Text, in a note (beside its code
# block, table and front matter too), an org note, a code tab, a note renamed between markdown and
# org, a notebook's cells and a diagram; the same HTML dropped on a note, an org note and a code
# tab.
drill_paste() {
	printf -- '---\ntitle: Note\n---\n\n# Note\n\nbefore\n\n```text\ncode\n```\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\nend\n' >"$vault/paste.md"
	echo "* Org" >"$vault/paste.org"
	echo "x = 1" >"$vault/paste.py"
	printf -- '# Drop\n\n```text\ncode\n```\n\ntext\n' >"$vault/drop.md"
	printf '* Drop\n\ntext\n' >"$vault/drop.org"
	cat >"$vault/paste.ipynb" <<'EOF'
{"cells": [{"cell_type": "markdown", "metadata": {}, "source": ["Notes\n"]},
 {"cell_type": "code", "execution_count": null, "metadata": {}, "outputs": [], "source": ["x = 1\n"]}],
 "metadata": {}, "nbformat": 4, "nbformat_minor": 5}
EOF
	cat >"$vault/paste.drawio" <<'EOF'
<mxfile><diagram id="p" name="Page-1"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/></root></mxGraphModel></diagram></mxfile>
EOF
	launch 90 ACCENT_BENCH_PASTE=1 >"$dir/paste"
	expect paste paste
}

# The page and line numbers the Outline's rows and thumbnails show under the real pointer, on a PDF,
# a diagram and a note the drill writes, a row's laid over its title on the row's own colour
# (DESIGN.md, Outline).
drill_outline_hover() {
	test -e "$pdfium" || { echo "SKIPPED the Outline hover drill: no libpdfium"; return 0; }
	launch 90 ACCENT_BENCH_OUTLINE=hover >"$dir/outline-hover"
	long='"A title that runs on well past the width of the sidebar"'
	has "$dir/outline-hover" \
		'bench outline hover hover.pdf thumb page=2 number=Some("2") corner=true' \
		'bench outline hover hover.pdf row="Introduction" hovered=true number=Some("2") drawn=true matched=true kept=true' \
		"bench outline hover hover.pdf row=$long hovered=true number=Some(\"3\") drawn=true matched=true kept=true" \
		'bench outline hover hover.pdf away thumb=Some(None) row="Introduction" drawn=Some(false) left_matched=Some(true)' \
		'bench outline hover hover.drawio thumb page=2 number=Some("2") corner=true' \
		'bench outline hover hover.drawio row="First" hovered=true number=Some("1") drawn=true matched=true kept=true' \
		"bench outline hover hover.drawio row=$long hovered=true number=Some(\"2\") drawn=true matched=true kept=true" \
		'bench outline hover hover.drawio away thumb=Some(None) row="First" drawn=Some(false) left_matched=Some(true)' \
		'bench outline hover hover.md row="Short" hovered=true number=Some("5") drawn=true matched=true kept=true' \
		"bench outline hover hover.md row=$long hovered=true number=Some(\"9\") drawn=true matched=true kept=true" \
		'bench outline hover parked row="Short" hovered=true number=Some("6") drawn=true matched=true' \
		"bench outline hover spliced numbers=[(\"Added\", Some(\"1\")), (\"Hover\", Some(\"3\")), (\"Short\", Some(\"7\")), ($long, Some(\"11\"))]" \
		'bench outline hover hover.md away thumb=None row="Short" drawn=Some(false) left_matched=Some(true)'
}

# A zoom while the view still coasts from a fling, whose offset GTK writes every frame in the pixels
# of the zoom it began at: each kept what it keeps, by a pinch, Ctrl+wheel and the chord alike
# (DESIGN.md, Zoom).
drill_coast() {
	launch 120 ACCENT_BENCH_COAST=1 >"$dir/coast"
	for case in 'diagram pinch' 'diagram wheel' 'diagram chord' 'image pinch' 'image wheel' \
		'image chord' 'notebook wheel' 'notebook chord' 'note wheel' 'note chord' 'preview wheel' \
		'preview chord'; do
		grep -q "^bench coast $case coasting=true zoomed=true kept=true " "$dir/coast" ||
			{ cat "$dir/coast"; return 1; }
	done
}

# The vault's root in each sort, with a folder left open staying open. Each file's creation time
# is the order it was written in, c, a then B, apart by more than a coarse clock's tick.
# Presentation's laser pointer over a presented diagram, PDF and note, put up and away by a real
# `L`: a drag's stroke held, fading, brought back by a press and gone, the document under it
# neither turning its page nor selecting, and `l` typed into the find bar and a code tab there,
# where the palette's Laser Pointer puts it up (DESIGN.md, Views).
drill_laser() {
	test -e "$pdfium" || { echo "SKIPPED the laser drill: no libpdfium"; return 0; }
	launch 90 ACCENT_BENCH_CHROME=laser >"$dir/laser"
	for kind in diagram pdf note; do
		has "$dir/laser" \
			"bench laser $kind on=true" \
			"bench laser $kind drawing strokes=1 strength=full marked=true" \
			"bench laser $kind held strokes=1 strength=full marked=true" \
			"bench laser $kind fading strokes=1 strength=fading" \
			"bench laser $kind again strokes=2 strength=full marked=true" \
			"bench laser $kind gone strokes=0 strength=none marked=false" \
			"bench laser $kind off on=false"
	done
	has "$dir/laser" \
		'bench laser diagram kept=true page=1 selected=[]' \
		'bench laser diagram clicked page=2 selected=[]' \
		'bench laser pdf kept=true page=Some("Page 1 of 1")' \
		'bench laser note kept=true scrollY=0 selected=Some(false)' \
		'bench laser find query="l" on=false' \
		'bench laser find closed on=true' \
		'bench laser left presenting=false on=false' \
		'bench laser code text=Some("x = 1l\n") on=false' \
		'bench laser code palette on=true' \
		'bench laser button before shown=true on=true active=true' \
		'bench laser button pressed shown=true on=false active=false' \
		'bench laser button again shown=true on=true active=true' \
		'bench laser code left on=false' \
		'bench laser button left shown=false on=false active=false'
}

# The keys that turn a presented diagram's page and a presented PDF's, each previous-page key
# mirroring its next-page one, on a deck of pages fitted shorter than half the view too, which a
# turn has to go on from at the top and the status bar names while presenting, reading naming the
# page under the middle. That deck read at Fit Height ends at its last page's bottom, and
# presenting it shows each page alone and centred, the wheel, Up, Down, Page Up and Page Down
# turning it too, while the page at the top stays there on the way in and out (DESIGN.md, Status
# bar, Views and Zoom).
drill_paging() {
	test -e "$pdfium" || { echo "SKIPPED the paging drill: no libpdfium"; return 0; }
	launch 90 ACCENT_BENCH_CHROME=paging >"$dir/paging"
	for kind in diagram pdf; do
		has "$dir/paging" \
			"bench paging $kind Right 2>3" "bench paging $kind Left 2>1" \
			"bench paging $kind space 2>3" "bench paging $kind shift+space 2>1" \
			"bench paging $kind n 2>3" "bench paging $kind p 2>1" \
			"bench paging $kind End 2>3" "bench paging $kind Home 2>1"
	done
	has "$dir/paging" \
		'bench paging diagram Page_Down 2>3' 'bench paging diagram Page_Up 2>1' \
		'bench paging wide range reading top=3 room=false status="Page 4 of 8"' \
		'bench paging wide reading Right 3>4 status="Page 5 of 8"' \
		'bench paging wide range presented top=3 room=false status="Page 3 of 8"' \
		'bench paging wide alone presented page=3 centred=true beside=0' \
		'bench paging wide Right 3>4 status="Page 4 of 8"' \
		'bench paging wide Left 3>2 status="Page 2 of 8"' \
		'bench paging wide space 3>4 status="Page 4 of 8"' \
		'bench paging wide shift+space 3>2 status="Page 2 of 8"' \
		'bench paging wide n 3>4 status="Page 4 of 8"' \
		'bench paging wide p 3>2 status="Page 2 of 8"' \
		'bench paging wide End 3>8 status="Page 8 of 8"' \
		'bench paging wide Home 3>1 status="Page 1 of 8"' \
		'bench paging wide end 8 "Page 8 of 8" Left 7 "Page 7 of 8"' \
		'bench paging wide notch_down 3>4 status="Page 4 of 8"' \
		'bench paging wide alone notch_down page=4 centred=true beside=0' \
		'bench paging wide notch_up 3>2 status="Page 2 of 8"' \
		'bench paging wide alone notch_up page=2 centred=true beside=0' \
		'bench paging wide Down 3>4 status="Page 4 of 8"' \
		'bench paging wide alone Down page=4 centred=true beside=0' \
		'bench paging wide Up 3>2 status="Page 2 of 8"' \
		'bench paging wide alone Up page=2 centred=true beside=0' \
		'bench paging wide Page_Down 3>4 status="Page 4 of 8"' \
		'bench paging wide alone Page_Down page=4 centred=true beside=0' \
		'bench paging wide Page_Up 3>2 status="Page 2 of 8"' \
		'bench paging wide alone Page_Up page=2 centred=true beside=0' \
		'bench paging wide range left top=3 room=false status="Page 4 of 8"'
}

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

# Links in the root, Show Hidden Files on: one to nothing marked, an editor's lock never listed
# (one there from the start and one made while open), a dot-named link to a file outside listed,
# and a link into the vault listed beside the file it names, dimmed with a link after its name,
# the index holding the file once; then the marked one listed so once its file is written; last
# a second link's name, which a link and Go to File find the file by, Go to File in one row
# though the window opened the link by its own path too.
drill_dangling() {
	mkdir "$vault/real"
	ln -s real/x.md "$vault/link.md"
	ln -s user@host.1:1 "$vault/.#note.md"
	echo "# Outside" >"$dir/outside.md"
	ln -s "$dir/outside.md" "$vault/.linked.md"
	echo "# Pre" >"$vault/real/pre.md"
	ln -s real/pre.md "$vault/pre.md"
	ln -s real/pre.md "$vault/shortcut.md"
	launch 60 ACCENT_BENCH_DANGLING=1 >"$dir/dangling"
	has "$dir/dangling" \
		'bench dangling open show_hidden=Some(true) link.md=b .#note.md=- .linked.md=fi pre.md=f+link real/pre.md=fi' \
		'bench dangling made gone.md=b .#other.md=-' \
		'bench dangling target link.md=f+link real/x.md=fi once=true' \
		'bench dangling alias followed=Some("real/pre.md")' \
		'bench dangling goto ["alias shortcut.md real/pre.md"]'
}

# The Graph tab opened on the whole vault, then unchecked round a note: the notes it links to and
# is linked from, a ghost for one not written yet, a press opening a note beside the graph as the
# other pane's preview with the keyboard left on the graph and growing the graph by its links,
# Info and Outline speaking for the note in front, the tab back with the session, its roots too,
# started again round a note it does not hold, settled in one frame with animations off, and
# opened on the whole vault again once closed.
drill_graph() {
	printf '# Alpha\n[[b]] [[c]] [[gone]] #topic ![[pic.png]]\n' >"$vault/a.md"
	echo '[[c]]' >"$vault/b.md"
	echo '[[f]]' >"$vault/c.md"
	echo 'alone' >"$vault/d.md"
	echo 'end' >"$vault/f.md"
	echo 'alone' >"$vault/lone.md"
	echo '[[a]]' >"$vault/e.md"
	echo 'import util' >"$vault/main.py"
	echo 'x = 1' >"$vault/util.py"
	cp "$repo/docs/guide/social-preview.png" "$vault/pic.png"
	launch 120 ACCENT_BENCH_GRAPH=check:a.md >"$dir/graph"
	launch 60 ACCENT_BENCH_GRAPH=restored >>"$dir/graph"
	expect graph
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
