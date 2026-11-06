#!/bin/sh
# Runs a command the way the headless checks launch accent, on the Xvfb `make xvfb` starts.
#
# A private session bus per run: accent is a single-instance GApplication, so without one a second
# invocation forwards its arguments to whatever instance is already up and exits 0, which makes a
# check pass while proving nothing. It starts no services, for the reasons headless-bus.conf gives.
# The XDG dirs point at a scratch directory, so a check neither reads the user's config nor writes
# a session and an index. TMPDIR too, where the terminal holder listens and every start sweeps for
# orphaned shells.
#
# Usage: build-aux/headless.sh [VAR=value…] <command> [args…]
# The command finds the scratch directory, which holds an empty vault, in $HEADLESS_DIR.
set -eu
repo=$(cd "$(dirname "$0")/.." && pwd)
# Under /tmp whatever the caller's TMPDIR is, because a unix socket path is at most 108 bytes.
dir=$(mktemp -d /tmp/accent-headless-XXXXXX)
trap 'rm -rf "$dir"' EXIT
mkdir -p "$dir/tmp" "$dir/vault" "$dir/config/glib-2.0/settings"
# The accent colour and the monospace font, which the drills print and a distribution may change
# (Ubuntu: orange, Ubuntu Sans Mono). libadwaita reads the accent from GSettings only with
# ADW_DISABLE_PORTAL. DejaVu Sans Mono is as wide as GNOME's Adwaita Mono, which Debian and Ubuntu
# do not package.
printf "[org/gnome/desktop/interface]\naccent-color='blue'\nmonospace-font-name='DejaVu Sans Mono 11'\n" \
	>"$dir/config/glib-2.0/settings/keyfile"
export HEADLESS_DIR="$dir" XDG_CONFIG_HOME="$dir/config" XDG_CACHE_HOME="$dir/cache" \
	XDG_STATE_HOME="$dir/state" XDG_DATA_HOME="$dir/data" TMPDIR="$dir/tmp" \
	GSETTINGS_BACKEND=keyfile ADW_DISABLE_PORTAL=1
# GDK_BACKEND=x11 because DISPLAY alone leaves GTK on a Wayland session; cairo because llvmpipe,
# Xvfb's GL, costs more CPU and memory; no a11y because the private bus has no registry;
# fatal-criticals so a GTK critical fails the check.
dbus-run-session --config-file="$repo/build-aux/headless-bus.conf" -- \
	env DISPLAY=":${DISPLAY_NUM:-99}" GDK_BACKEND=x11 GSK_RENDERER=cairo GTK_A11Y=none \
	G_DEBUG=fatal-criticals "$@"
