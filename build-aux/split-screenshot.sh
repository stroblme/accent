#!/bin/sh
# Join a light and a dark screenshot of the same window along the diagonal
# from the top-left to the bottom-right corner: light below it, dark above,
# as in the README's screenshots.
#
# Usage: build-aux/split-screenshot.sh <light.png> <dark.png> <out.png>
set -eu
[ $# -eq 3 ] || { echo "usage: $0 <light.png> <dark.png> <out.png>" >&2; exit 2; }
size=$(magick identify -format '%wx%h' "$1")
w=${size%x*} h=${size#*x}
# Cut the dark shot down to the upper-right triangle (DstIn keeps it where the
# antialiased triangle is opaque; `-alpha set` gives an opaque capture the alpha
# that takes) and lay it over the light one.
magick "$1" \( "$2" -alpha set \( -size "$size" xc:none -fill white \
	-draw "polygon 0,0 $w,0 $w,$h" \) -compose DstIn -composite \) \
	-compose Over -composite "$3"
