#!/bin/sh
# Add libadwaita's rounded corners and focused shadow to a compositor-free capture.
#
# Usage: build-aux/frame-screenshot.sh <scale> <in.png> <out.png>
set -eu
[ $# -eq 3 ] || { echo "usage: $0 <scale> <in.png> <out.png>" >&2; exit 2; }
s=$1
size=$(magick identify -format '%wx%h' "$2")
w=${size%x*} h=${size#*x}
r=$((15 * s)) m=$((40 * s))
# One CSS box-shadow layer, `0 0 <blur> <spread> rgba(0, 0, 0, <alpha>)`: the window's shape grown
# by the spread and blurred by half the blur radius, as a Gaussian's sigma.
layer() { # <blur> <spread> <alpha>
	echo "( -clone 0 -alpha extract -morphology Dilate Disk:$(($2 * s)) -blur 0x$(($1 * s / 2)) \
		-evaluate multiply $3 -background black -alpha shape )"
}
# shellcheck disable=SC2046 # each layer is several words on purpose
magick "$2" -alpha set \( -size "$size" xc:black -fill white \
	-draw "roundrectangle 0,0,$((w - 1)),$((h - 1)),$r,$r" \) -alpha off \
	-compose CopyOpacity -composite -compose Over -bordercolor none -border "$m" \
	$(layer 14 5 0.15) $(layer 5 2 0.10) $(layer 0 1 0.05) \
	-reverse -background none -flatten "$3"
