#!/bin/sh
# Regenerate README and metainfo screenshots from the generated vault. Each scene launches the
# ACCENT_BENCH_SHOT drill on private Xvfb in light and dark themes at scale 2. Join the pair for
# the README, use light alone for metainfo, and add GNOME corners and shadow.
#
# Build an isolated home, merl-rt stand-in, Git history, and stock GNOME settings under
# target/screenshots.
#
# Usage: build-aux/screenshots.sh <directory holding accent and accent-cli>
set -eu
[ $# -eq 1 ] || { echo "usage: $0 <bin_dir>" >&2; exit 2; }
for tool in magick htop Xvfb dbus-run-session fuser git python3; do
	command -v "$tool" >/dev/null || { echo "$0 needs $tool" >&2; exit 1; }
done
python3 -c 'import debugpy' 2>/dev/null || { echo "$0 needs debugpy for scene 5" >&2; exit 1; }
bin=$(cd "$1" && pwd)
test -x "$bin/accent" && test -x "$bin/accent-cli" ||
	{ echo "no accent and accent-cli in $bin: run make all" >&2; exit 1; }
repo=$(cd "$(dirname "$0")/.." && pwd)
here=$repo/build-aux/screenshots
root=$repo/target/screenshots
vault=$root/testvault
test -d "$vault" || { echo "no $vault: make screenshots generates it" >&2; exit 1; }

# Git, the shell and htop read nothing of the user's.
export HOME="$root" GIT_CONFIG_NOSYSTEM=1
mkdir -p "$root/bin" "$root/out" "$root/xdg/config/accent" "$root/xdg/config/gtk-4.0"
cp "$here/bashrc" "$root/.bashrc"
# merl-rt is found on PATH, and what it suggests depends on the whole vault: the stand-in answers
# one fixed sentence.
printf '#!/bin/sh\nexec python3 "%s" --inline "Calibration follows."\n' \
	"$repo/build-aux/fake-lsp.py" >"$root/bin/merl-rt"
chmod +x "$root/bin/merl-rt"
printf 'line_numbers = true\ncolumn_width = 85\nword_suggestions = false\n' \
	>"$root/xdg/config/accent/config.toml"
# GNOME's interface font, which no settings daemon tells GTK under Xvfb, and no caret blink or
# animation to be caught half way.
printf '[Settings]\ngtk-font-name=Adwaita Sans 11\ngtk-cursor-blink=false\n%s\n%s\n' \
	gtk-enable-animations=false gtk-decoration-layout=:minimize,maximize,close \
	>"$root/xdg/config/gtk-4.0/settings.ini"

# The vault is its own repository: a history of a few days in real chunks, a branch merged back
# in, an upstream four commits behind, and Code/analyze.py staged and changed again since.
echo "(Persistent) Split Terminals" >"$vault/whatelse"
cp "$here/analyze.py" "$here/readout.py" "$vault/Code/"
git -C "$vault" init -q -b main
export GIT_AUTHOR_NAME="Alex Example" GIT_AUTHOR_EMAIL=alex@example.org \
	GIT_COMMITTER_NAME="Alex Example" GIT_COMMITTER_EMAIL=alex@example.org
now=$(date +%s)
commit() { # <minutes ago> <message> <pathspec>...
	at="@$((now - $1 * 60)) +0000" message=$2
	shift 2
	git -C "$vault" add -A -- "$@"
	GIT_AUTHOR_DATE=$at GIT_COMMITTER_DATE=$at git -C "$vault" commit -q -m "$message"
}
commit 13000 "Import the notes" . ':!Attachments' ':!Daily' ':!Events' ':!Inbox' ':!Submissions' \
	':!Code'
commit 11500 "attachments: scans, figures and sweeps" Attachments
commit 8700 "daily: september" Daily
commit 7300 "events and inbox" Events Inbox
commit 5800 "submissions: earlier projects" Submissions ':!Submissions/grant-2026'
commit 4400 "submissions: grant 2026" Submissions/grant-2026
commit 1560 "code: sweep driver" Code/build.sh Code/Makefile
commit 1500 "code: project metadata" Code/pyproject.toml Code/tasks.json
commit 290 "code: sweep statistics" Code/analyze.py Code/readout.py
# The plots on a branch of their own while the readme went in, then merged.
git -C "$vault" switch -q -c plots
commit 47 "code: plots" Code/plot.py
git -C "$vault" switch -q main
commit 30 "code: readme and licence" Code
at="@$((now - 12 * 60)) +0000"
GIT_AUTHOR_DATE=$at GIT_COMMITTER_DATE=$at git -C "$vault" merge -q --no-edit plots
# A remote the pane's fetch on opening can reach, borrowing the vault's objects.
git init -q --bare -b main "$root/origin.git"
echo "$vault/.git/objects" >"$root/origin.git/objects/info/alternates"
git -C "$root/origin.git" update-ref refs/heads/main "$(git -C "$vault" rev-parse HEAD~3)"
git -C "$vault" remote add origin "$root/origin.git"
git -C "$vault" fetch -q origin
git -C "$vault" branch -q -u origin/main
sed -i '1s/the noise-atlas notes/the noise-atlas and grant notes/' "$vault/Code/analyze.py"
git -C "$vault" add Code/analyze.py
git -C "$vault" apply "$here/analyze.patch"
test "$(git -C "$vault" status --porcelain)" = "MM Code/analyze.py" ||
	{ git -C "$vault" status --short | head >&2; echo "not the changes scene 2 needs" >&2; exit 1; }

# Room for the 1000x700 window at scale 2, and for the pointer parked past its corner.
display=:${SHOT_DISPLAY:-98}
socket=/tmp/.X11-unix/X${display#:}
test ! -e "$socket" || { echo "display $display is taken: set SHOT_DISPLAY" >&2; exit 1; }
Xvfb "$display" -screen 0 2160x1560x24 -nolisten tcp >/dev/null 2>&1 &
xvfb=$!
trap 'kill $xvfb' EXIT
trap 'exit 1' INT TERM
for _ in 1 2 3 4 5 6 7 8 9 10; do test -S "$socket" && break; sleep 0.5; done

# One launch per scene, as `make smoke` launches: scratch XDG dirs and TMPDIR outside a private
# bus, which starts no services (headless-bus.conf says why). The index is shared and the session
# is not.
mkdir -p "$repo/data/screenshots"
n=0
for name in files git terminals focus debug; do
	n=$((n + 1))
	tmp=$(mktemp -d /tmp/accent-shot-XXXXXX)
	rm -rf "$root/xdg/state"
	status=0
	env SHELL=/bin/bash PATH="$root/bin:$PATH" TMPDIR="$tmp" \
		XDG_CONFIG_HOME="$root/xdg/config" XDG_CACHE_HOME="$root/xdg/cache" \
		XDG_STATE_HOME="$root/xdg/state" XDG_DATA_HOME="$root/xdg/data" \
		GSETTINGS_BACKEND=memory ADW_DISABLE_PORTAL=1 \
		dbus-run-session --config-file="$repo/build-aux/headless-bus.conf" -- \
		env DISPLAY="$display" GDK_BACKEND=x11 GDK_SCALE=2 GSK_RENDERER=cairo \
		GTK_A11Y=none G_DEBUG=fatal-criticals \
		ACCENT_BENCH_SHOT="$n:$root/out" timeout 300 "$bin/accent" "$vault" || status=$?
	# Scene 3 ends its shells, and the holder leaves with the last; one a failed scene left behind
	# still has its socket. Bounded: fuser stats every mount, and hangs on an unreachable one.
	held=$tmp/accent-$(id -u)
	test ! -S "$held/hold-2.sock" || timeout 60 fuser -k "$held/hold-2.lock" >/dev/null 2>&1 || true
	rm -rf "$tmp"
	test "$status" = 0 || { echo "scene $n failed with $status" >&2; exit 1; }
	"$repo/build-aux/split-screenshot.sh" "$root/out/shot-$n-light.png" \
		"$root/out/shot-$n-dark.png" "$root/out/shot-$n-split.png"
	"$repo/build-aux/frame-screenshot.sh" 2 "$root/out/shot-$n-split.png" \
		"$repo/docs/screenshot-$n.png"
	"$repo/build-aux/frame-screenshot.sh" 2 "$root/out/shot-$n-light.png" \
		"$repo/data/screenshots/$name.png"
done
