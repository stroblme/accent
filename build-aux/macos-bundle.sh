#!/bin/bash
# Build Accent.app holding every library, module and data file the app loads, so it runs on a Mac
# without MacPorts or Homebrew. `make app` runs this; Xcode's command line tools are all it uses.
#
# The libraries go to Contents/Frameworks, relinked through @rpath. GLib's, gdk-pixbuf's and GIO's
# data and modules go to Contents/Resources, which `apps/gtk/src/platform/bundle.rs` points them
# at, the paths built into them being the build machine's. libpdfium and the remote servers go to
# Contents/lib/accent, the `../lib/accent` beside the binary where the app looks for them anyway.
# No spell checking: the packaged enchant looks for its providers under its own prefix only
# (NOTEPAD.md, macOS).
#
# Usage: build-aux/macos-bundle.sh <directory holding accent, accent-cli and accent.icns> <version>
set -eu
[ $# -eq 2 ] || { echo "usage: $0 <bin_dir> <version>" >&2; exit 2; }
bin=$1 version=$2
app_id=io.github.stroblme.Accent
app=$bin/Accent.app
c=$app/Contents
res=$c/Resources
# The package manager's prefix: /opt/local for MacPorts, `brew --prefix` for Homebrew.
prefix=${PREFIX:-$(dirname "$(dirname "$(command -v glib-compile-schemas)")")}

rm -rf "$app"
mkdir -p "$c/MacOS" "$c/Frameworks" "$c/lib/accent" "$res/lib/gdk-pixbuf/loaders" \
	"$res/lib/gio/modules" "$res/share/glib-2.0/schemas" "$res/share/icons/Adwaita" \
	"$res/share/icons/hicolor/scalable/apps" "$res/share/icons/hicolor/symbolic/apps"
# The package manager builds its libraries for the macOS it runs on, which is then the app's minimum.
sed -e "s/@VERSION@/$version/" -e "s/@MINOS@/$(sw_vers -productVersion | cut -d. -f1).0/" \
	data/Info.plist >"$c/Info.plist"
cp "$bin/accent" "$bin/accent-cli" "$c/MacOS/"
cp "$bin/accent.icns" "$res/"
cp vendor/pdfium/libpdfium.dylib "$c/lib/accent/"
# Best effort, as `make install`: `make server` needs a musl toolchain, and only a remote vault
# needs its output, which sits beside this profile's folder.
for arch in x86_64 aarch64; do
	server=$(dirname "$bin")/$arch-unknown-linux-musl/$(basename "$bin")/accent-cli
	for suffix in "" .xz .xz.sha256; do
		if [ -f "$server$suffix" ]; then cp "$server$suffix" "$c/lib/accent/accent-cli-$arch$suffix"; fi
	done
done

# gdk-pixbuf's loaders, the cache naming them by @executable_path: dlopen() resolves it, and
# GModule passes on a path it cannot find as it is.
cache=$(pkg-config --variable=gdk_pixbuf_cache_file gdk-pixbuf-2.0)
for so in $(sed -n 's/^"\(\/.*\.so\)"$/\1/p' "$cache"); do cp "$so" "$res/lib/gdk-pixbuf/loaders/"; done
sed 's|^"/.*/\([^/]*\.so\)"$|"@executable_path/../Resources/lib/gdk-pixbuf/loaders/\1"|' "$cache" \
	>"$res/lib/gdk-pixbuf/loaders.cache"
# GIO's TLS backend, for a diagram's pictures over https.
cp "$prefix/lib/gio/modules/libgiognutls.so" "$res/lib/gio/modules/"
chmod -R u+w "$c"

# Each library the binary and the modules load, copied once into Frameworks under the name it is
# linked by, and every reference to it pointed at @rpath, which is Frameworks for the binary and
# whatever it loads. ponytail: a worklist split on whitespace; no path here has a space in it.
todo="$c/MacOS/accent $(find "$res/lib" -name '*.so')"
while [ -n "$todo" ]; do
	set -- $todo
	f=$1
	shift
	todo=$*
	for dep in $(otool -L "$f" | tail -n +2 | awk '{print $1}'); do
		case $dep in /System/* | /usr/lib/* | @*) continue ;; esac
		name=${dep##*/}
		if [ ! -e "$c/Frameworks/$name" ]; then
			cp "$dep" "$c/Frameworks/"
			chmod u+w "$c/Frameworks/$name"
			install_name_tool -id "@rpath/$name" "$c/Frameworks/$name"
			todo="$todo $c/Frameworks/$name"
		fi
		install_name_tool -change "$dep" "@rpath/$name" "$f"
	done
	# A library's own rpath into the build machine (MacPorts' brotli has /opt/local/lib) would
	# find the package manager's copy before ours wherever there is one.
	for rpath in $(otool -l "$f" | awk '/cmd LC_RPATH/ { getline; getline; print $2 }'); do
		case $rpath in @*) ;; *) install_name_tool -delete_rpath "$rpath" "$f" ;; esac
	done
done
install_name_tool -add_rpath @executable_path/../Frameworks "$c/MacOS/accent"
# libpdfium comes stripped.
strip -x "$c/MacOS/accent" "$c/MacOS/accent-cli" "$c"/Frameworks/* "$res"/lib/*/*/*.so

# GTK's own schemas, which its file, colour and emoji choosers abort without, and the interface
# ones libadwaita reads as it does from MacPorts. XDG_DATA_DIRS finds them under share/.
schemas=$prefix/share/glib-2.0/schemas
cp "$schemas"/org.gtk.gtk4.*.gschema.xml "$schemas/org.gnome.desktop.interface.gschema.xml" \
	"$schemas/org.gnome.desktop.a11y.interface.gschema.xml" "$schemas/org.gnome.desktop.enums.xml" \
	"$res/share/glib-2.0/schemas/"
glib-compile-schemas "$res/share/glib-2.0/schemas"
# The Adwaita icons without its full-colour raster sizes and cursors, which the app does not draw
# (AppKit's cursors stand in); hicolor, GTK's fallback, with only the app's own icons.
for item in "$prefix/share/icons/Adwaita"/*; do
	case ${item##*/} in cursors | [0-9]*x[0-9]*) ;; *) cp -R "$item" "$res/share/icons/Adwaita/" ;; esac
done
cp "$prefix/share/icons/hicolor/index.theme" "$res/share/icons/hicolor/"
cp "data/icons/hicolor/scalable/apps/$app_id.svg" "$res/share/icons/hicolor/scalable/apps/"
cp "data/icons/hicolor/symbolic/apps/$app_id-symbolic.svg" "$res/share/icons/hicolor/symbolic/apps/"
gtk4-update-icon-cache -q -t -f "$res/share/icons/Adwaita"
gtk4-update-icon-cache -q -t -f "$res/share/icons/hicolor"

# Signed ad hoc, inside out: Apple Silicon runs no unsigned arm64 code, and relinking voids the
# linker's signature. Gatekeeper still asks on the first launch, there being no Developer ID.
codesign -f -s - "$c"/Frameworks/* "$res"/lib/*/*/*.so "$c/lib/accent/libpdfium.dylib" "$c/MacOS/accent-cli"
codesign -f -s - "$app"

# Nothing may still point into the build machine: libpdfium's own `./libpdfium.dylib` id aside.
status=0
for f in "$c"/MacOS/* "$c"/Frameworks/* "$c"/lib/accent/*.dylib "$res"/lib/*/*/*.so; do
	left=$(otool -L "$f" | tail -n +2 | awk '{print $1}' | grep -v '^/System/\|^/usr/lib/\|^@\|^\./libpdfium' || true)
	left="$left$(otool -l "$f" | awk '/cmd LC_RPATH/ { getline; getline; print " rpath " $2 }' | grep -v ' @' || true)"
	if [ -n "$left" ]; then echo "$f still names: $left" >&2; status=1; fi
done
codesign --verify --deep --strict "$app"
[ $status = 0 ] && echo "Built $app ($(du -sh "$app" | cut -f1))"
exit $status
