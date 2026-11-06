# accent: convenience wrapper around cargo and the packaging bits.
#
# Cargo is the build system; this file exists for the things cargo does not do, mostly
# installing data files and driving the headless checks. `make help` lists everything.
#
# The GTK app is not in the workspace's default-members (it needs gtk4/libadwaita dev headers),
# so every target that touches it names `-p accent` explicitly.

APP_ID    := io.github.stroblme.Accent
CARGO     ?= cargo
PROFILE   ?= release
# Installs into the user's home by default, so `make install` never needs sudo and the app still
# shows up in the GNOME app grid. Override PREFIX=/usr/local (with sudo) or DESTDIR for packaging.
DESTDIR   ?=
PREFIX    ?= $(HOME)/.local
BINDIR    ?= $(PREFIX)/bin
LIBDIR    ?= $(PREFIX)/lib
DATADIR   ?= $(PREFIX)/share
# Not empty on macOS, which builds and installs a little differently (`gtk`, `install`).
DARWIN    := $(filter Darwin,$(shell uname -s))
# Where macOS's `install` puts Accent.app: Launchpad and Spotlight list a per-user folder too.
APPSDIR   ?= $(HOME)/Applications
# The workspace's, for Accent.app's Info.plist and the disk image's name.
VERSION   := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml)
# Honours CARGO_TARGET_DIR, which parallel worktrees must each set to a directory of their
# own: without this `smoke` and `install` look for a binary cargo never wrote there.
TARGET_ROOT := $(or $(CARGO_TARGET_DIR),target)

# libpdfium is a 7 MB binary, so it is not in git: `make pdfium` fetches the matching build from
# bblanchon/pdfium-binaries. Bump PDFIUM_BUILD and every checksum below it together; the release
# publishes one per asset.
PDFIUM_BUILD  := 8076
PDFIUM_RELEASE := https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F$(PDFIUM_BUILD)
# The desktop build follows the machine rather than assuming x86-64 Linux.
PDFIUM_ARCH   := $(if $(filter aarch64 arm64,$(shell uname -m)),arm64,x64)
PDFIUM_OS     := $(if $(DARWIN),mac,linux)
PDFIUM_URL    := $(PDFIUM_RELEASE)/pdfium-$(PDFIUM_OS)-$(PDFIUM_ARCH).tgz
PDFIUM_SHA256_linux-x64   := d9d67bc40af03aef4fe28a60b19b1086f28ace019c8c9caf19cb7fe3d14ceca3
PDFIUM_SHA256_linux-arm64 := d7247b33ae5545615a5e877235dd97afc879e3a8805689684f528cae3339d352
PDFIUM_SHA256_mac-x64     := 40865f34642c34d82cc336132df9e0347133f4692cd46776647af160f9a5cca9
PDFIUM_SHA256_mac-arm64   := 0d6781fe08906baff3d82c90953e519fbc4eb253fe76431e5ed53b157763b97c
PDFIUM_SHA256 := $(PDFIUM_SHA256_$(PDFIUM_OS)-$(PDFIUM_ARCH))
PDFIUM_LIB    := vendor/pdfium/libpdfium.$(if $(DARWIN),dylib,so)
# macOS has `shasum`, not `sha256sum`.
SHA256_CHECK  := $(if $(DARWIN),shasum -a 256 -c -,sha256sum -c -)

# Android. The two ABIs the APK ships: the phone, and the emulator this machine can run.
# `jniLibs/<abi>` is where Gradle packs a bare `.so` into the APK, and where Android's linker
# then finds it by name — which is how `Pdfium::bind_to_system_library` gets hold of it.
ANDROID_ABIS  := arm64-v8a x86_64
JNI_LIBS      := android/app/src/main/jniLibs
PDFIUM_SHA256_android_arm64-v8a := b7a6919dc130dfdf7e409bd1762ed2a306408c4faee1173668837fc446217bf7
PDFIUM_SHA256_android_x86_64    := 9d043689096c7cdd4ca98fff95599be115b2a38feba0c68d2a79ba5eedbda415
# `jniLibs` is named by ABI, the pdfium release by architecture.
PDFIUM_ASSET_arm64-v8a := arm64
PDFIUM_ASSET_x86_64    := x64
ANDROID_TARGETS := aarch64-linux-android x86_64-linux-android
# A release cross build takes `[profile.android]` from Cargo.toml — release plus `strip`, because
# the `.so` ships in the APK, and at release's speed, which a phone's first index needs more than
# the 1.7 MB a size build saved (Cargo.toml says what was measured).
ANDROID_PROFILE_FLAG := $(if $(filter release,$(PROFILE)),--profile android,)
# uniffi keeps its metadata in the symbol table, which that profile strips, so the bindings are
# read out of a host build of the same crate — the flow uniffi documents. Dev profile, because
# that is the one the generator itself is built with.
HOST_FFI_LIB := $(TARGET_ROOT)/debug/libaccent_android.so
# The NDK's own readelf, for the 16 KB page-size check; the host's would do, but the NDK is what
# a machine building for Android is guaranteed to have.
ANDROID_NDK_HOME ?= $(firstword $(wildcard $(HOME)/Android/Sdk/ndk/*))
READELF ?= $(ANDROID_NDK_HOME)/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-readelf

# The remote server is this same CLI, built static so it starts on a host older than this one,
# once per machine `uname -m` names, and a connection uploads the one its host runs. Its name on
# the remote is the builds' blake3, so a rebuild re-provisions exactly once.
SERVER_ARCHES := x86_64 aarch64
SERVER_BIN     = $(TARGET_ROOT)/$(1)-unknown-linux-musl/$(PROFILE)/accent-cli

# `cargo build` puts a release build under target/release and a dev build under target/debug.
CARGO_PROFILE_FLAG := $(if $(filter release,$(PROFILE)),--release,)
TARGET_DIR := $(TARGET_ROOT)/$(PROFILE)

# The vault used by every test and benchmark. Never point these at a real vault.
VAULT     ?= testvault
VAULT_NOTES ?= 3600
VAULT_FILES ?= 40000
# Repo tooling rather than a CLI subcommand, so the fixtures are not a public feature of the app.
GEN_VAULT := $(wildcard crates/core/examples/gen-vault/*.rs)

# Headless runs need an X server; :99 is what CI uses. build-aux/headless.sh launches on it.
export DISPLAY_NUM ?= 99

.DEFAULT_GOAL := all
.PHONY: all core gtk gtk-bench clean distclean install uninstall test test-pdf check fmt fmt-check \
        clippy doc run smoke drills xvfb vault validate icons flatpak cargo-sources pdfium server \
        help requirements install-requirements android android-check android-test android-tools apk apk-release \
        pdfium-android bindings screenshots docs app dmg release pkgroot deb rpm

## all: build everything, core plus the desktop app
all: core gtk

## core: build accent-core, accent-api and accent-cli
core:
	$(CARGO) build $(CARGO_PROFILE_FLAG)

# MacPorts on macOS 12 has only VTE 0.76: there the app leaves out the `termprops` feature unless
# pkg-config finds 0.78 (apps/gtk/Cargo.toml).
GTK_FEATURES = $(if $(DARWIN),--no-default-features \
	$(if $(shell pkg-config --atleast-version=0.78 vte-2.91-gtk4 && echo y),--features termprops))

## gtk: build the desktop app (needs gtk4, libadwaita, gtksourceview5, libsoup-3.0, libspelling, vte-2.91-gtk4 dev packages)
gtk:
	$(CARGO) build $(CARGO_PROFILE_FLAG) -p accent $(GTK_FEATURES)

## gtk-bench: build the desktop app with the ACCENT_BENCH_* drills in, which a shipped build leaves out
#
# Same path as `gtk`'s binary: cargo keeps both builds and puts back whichever was asked for last.
gtk-bench:
	$(CARGO) build $(CARGO_PROFILE_FLAG) -p accent --features bench

## requirements: check what building and running the desktop app needs (installs nothing)
#
# One line per missing item; `install-requirements` installs them per distro. The library
# floors are what the binding features in apps/gtk/Cargo.toml ask pkg-config for (`v4_18` is
# gtk4 >= 4.18) and HarfBuzz's what crates/math/build.rs asks for.
# An optional item only warns: the app runs without it and lacks the feature it names.
RUST_MIN := $(shell sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml)
# On macOS VTE 0.78 is optional, as `gtk` builds without what it is for there.
GTK_LIBS := gtk4:4.18 libadwaita-1:1.8 gtksourceview-5:5.18 harfbuzz:8.3.1 libsoup-3.0:3.0 \
            $(if $(DARWIN),vte-2.91-gtk4:0.76,vte-2.91-gtk4:0.78) libspelling-1:0.1
OPTIONAL_LIBS := $(if $(DARWIN),vte-2.91-gtk4:0.78:termprops)
requirements:
	@missing=0; \
	for tool in cargo cc pkg-config glib-compile-resources; do \
		command -v $$tool >/dev/null || { echo "missing: $$tool"; missing=1; }; \
	done; \
	rust=$$(rustc --version 2>/dev/null | cut -d' ' -f2); \
	printf '%s\n' $(RUST_MIN) "$$rust" | sort -VC || \
		{ echo "missing: Rust >= $(RUST_MIN), found $${rust:-none}"; missing=1; }; \
	for lib in $(GTK_LIBS); do \
		name=$${lib%:*} min=$${lib#*:}; \
		pkg-config --atleast-version=$$min $$name 2>/dev/null || { missing=1; \
			echo "missing: $$name >= $$min, found $$(pkg-config --modversion $$name 2>/dev/null || echo none)"; }; \
	done; \
	for lib in $(OPTIONAL_LIBS); do \
		name=$${lib%%:*} min=$${lib#*:}; min=$${min%:*}; \
		pkg-config --atleast-version=$$min $$name 2>/dev/null || \
			echo "optional: $$name >= $$min, for the $${lib##*:} feature (apps/gtk/Cargo.toml)"; \
	done; \
	test -f $(PDFIUM_LIB) || echo "optional: libpdfium, for the PDF tab: run make pdfium"; \
	command -v git >/dev/null || echo "optional: git, for the Git pane"; \
	command -v ssh >/dev/null || echo "optional: ssh, for remote vaults"; \
	{ command -v cargo-zigbuild && command -v zig; } >/dev/null || \
		echo "optional: cargo-zigbuild and zig, for make server, which remote vaults need"; \
	python3 -c 'import lzma' >/dev/null 2>&1 || \
		echo "optional: Python 3 with lzma, for make server's compressed uploads"; \
	command -v merl-rt >/dev/null || echo "optional: merl-rt, for ghost text"; \
	fc-list -q "DejaVu Sans Mono" 2>/dev/null || echo "optional: DejaVu Sans Mono, which make drills is written against"; \
	if test $$missing = 0; then echo "All requirements met."; \
	else echo "make install-requirements installs them on openSUSE, Fedora, Debian, Ubuntu and Arch."; exit 1; fi

# What `requirements` checks for, as each distribution packages it, plus git, ssh and curl. The one
# list README.md, install.sh and CI rely on. GNOME 49 or newer is what has the versions above.
PKGS_suse   := gcc make pkgconf-pkg-config glib2-devel gtk4-devel libadwaita-devel gtksourceview5-devel \
               libsoup-devel vte-devel libspelling-devel dejavu-fonts git-core openssh-clients curl
PKGS_fedora := gcc make pkgconf-pkg-config glib2-devel gtk4-devel libadwaita-devel gtksourceview5-devel \
               libsoup3-devel vte291-gtk4-devel libspelling-devel dejavu-sans-mono-fonts git \
               openssh-clients curl
PKGS_debian := build-essential pkgconf libgio-2.0-dev-bin libgtk-4-dev libadwaita-1-dev \
               libgtksourceview-5-dev libsoup-3.0-dev libvte-2.91-gtk4-dev libspelling-1-dev \
               fonts-dejavu-mono git openssh-client curl
PKGS_arch   := base-devel gtk4 libadwaita gtksourceview5 libsoup3 vte4 libspelling ttf-dejavu \
               git openssh curl
# Root (a CI container) installs directly; anyone else through sudo.
SUDO := $(if $(filter 0,$(shell id -u)),,sudo)

## install-requirements: install the desktop requirements with the distribution's package manager
#
# Without asking: install.sh runs this with no terminal on stdin.
install-requirements:
	@. /etc/os-release; case " $$ID $$ID_LIKE " in \
	*suse*) $(SUDO) zypper --non-interactive install $(PKGS_suse) ;; \
	*" fedora "*) $(SUDO) dnf install -y $(PKGS_fedora) ;; \
	*" debian "* | *" ubuntu "*) $(SUDO) apt-get update && $(SUDO) apt-get install -y $(PKGS_debian) ;; \
	*" arch "*) $(SUDO) pacman -S --needed --noconfirm $(PKGS_arch) ;; \
	*) echo "No package list for $$ID: make requirements names what is missing."; exit 1 ;; \
	esac

## test: run the whole workspace test suite, desktop app included
test:
	$(CARGO) test --workspace --locked

## test-pdf: run the PDF tests too (needs libpdfium, see vendor/pdfium or ACCENT_PDFIUM_DIR)
test-pdf: pdfium
	$(CARGO) test -p accent-core -p accent-api --features pdf --locked

## pdfium: fetch libpdfium into vendor/pdfium (does nothing if it is already there)
pdfium: | $(PDFIUM_LIB)
$(PDFIUM_LIB):
	@mkdir -p $(dir $(PDFIUM_LIB))
	curl -fL --retry 3 -o $(PDFIUM_LIB).tgz "$(PDFIUM_URL)"
	echo "$(PDFIUM_SHA256)  $(PDFIUM_LIB).tgz" | $(SHA256_CHECK)
	@# The release keeps the library under lib/ and its metadata at the top level; we want the
	@# three files side by side, and none of the headers.
	tar -xzf $(PDFIUM_LIB).tgz -C $(dir $(PDFIUM_LIB)) --strip-components=1 lib/$(notdir $(PDFIUM_LIB))
	tar -xzf $(PDFIUM_LIB).tgz -C $(dir $(PDFIUM_LIB)) LICENSE VERSION
	rm -f $(PDFIUM_LIB).tgz

# --------------------------------------------------------------------------------------- Android
#
# The APK carries two shared libraries per ABI: `libaccent_android.so`, which is the Rust core
# and its uniffi scaffolding, and `libpdfium.so`. Both land in `jniLibs/<abi>/`, which Gradle
# packs into the APK and Android's linker reads by name.

## android-tools: install what a cross build needs (rust targets, cargo-ndk)
android-tools:
	rustup target add $(ANDROID_TARGETS)
	@command -v cargo-ndk >/dev/null || $(CARGO) install cargo-ndk --locked

## android: cross-build the Rust core for every Android ABI into the APK's jniLibs
android:
	@command -v cargo-ndk >/dev/null || { echo "cargo-ndk is missing: run 'make android-tools'"; exit 1; }
	$(CARGO) ndk $(foreach abi,$(ANDROID_ABIS),-t $(abi)) -o $(JNI_LIBS) \
		build $(ANDROID_PROFILE_FLAG) -p accent-android
	@# cargo-ndk copies every shared object the build produced. `pdfium-render` emits one of its
	@# own that nothing links or loads — we reach libpdfium through dlopen — so it would be half a
	@# megabyte of APK for nothing.
	rm -f $(JNI_LIBS)/*/libpdfium_render-*.so

## bindings: regenerate the Kotlin bindings from a host build of the FFI crate
bindings:
	$(CARGO) build -q -p accent-android --features cli
	$(CARGO) run -q -p accent-android --features cli --bin uniffi-bindgen -- \
		generate --library $(HOST_FFI_LIB) \
		--language kotlin --out-dir android/app/build/generated/uniffi

## pdfium-android: fetch libpdfium for every Android ABI into the APK's jniLibs
pdfium-android: $(ANDROID_ABIS:%=$(JNI_LIBS)/%/libpdfium.so)
$(JNI_LIBS)/%/libpdfium.so:
	@mkdir -p $(@D)
	curl -fL --retry 3 -o $(@D)/pdfium.tgz "$(PDFIUM_RELEASE)/pdfium-android-$(PDFIUM_ASSET_$*).tgz"
	echo "$(PDFIUM_SHA256_android_$*)  $(@D)/pdfium.tgz" | sha256sum -c -
	tar -xzf $(@D)/pdfium.tgz -C $(@D) --strip-components=1 lib/libpdfium.so
	rm -f $(@D)/pdfium.tgz

## android-test: run the app's own unit tests (needs a JDK and the SDK)
android-test:
	cd android && ./gradlew testDebugUnitTest

## apk: build the debug APKs, one per ABI (Gradle runs the cross build and the bindings itself)
apk: pdfium-android
	cd android && ./gradlew assembleDebug

## apk-release: build the release APKs, one per ABI
#
# Unsigned, on purpose: whoever publishes a build signs it with their own key, which is how
# F-Droid and a GitHub release both work. `apksigner sign` is the last step, not this file's.
apk-release: pdfium-android
	cd android && ./gradlew assembleRelease

## android-check: the Android gate — lint the bindings on the host, then cross-build them
android-check:
	$(CARGO) fmt --all --check
	$(CARGO) clippy -p accent-api --features android --all-targets --locked -- -D warnings
	$(CARGO) test -p accent-api --features android --locked
	$(MAKE) android
	@# Android 15 runs some devices with 16 KB pages, and a library laid out for 4 KB will not
	@# load there at all. Every LOAD segment must be aligned to 0x4000. No segment at all fails
	@# too: sh has no pipefail, so a missing readelf would otherwise pass every library.
	@for abi in $(ANDROID_ABIS); do \
		for so in $(JNI_LIBS)/$$abi/*.so; do \
			test -f "$$so" || continue; \
			$(READELF) -lW "$$so" | awk -v f="$$so" -v r="$(READELF)" '/LOAD/ { n++; if ($$NF != "0x4000") { print f " is not 16 KB aligned: " $$NF; bad=1 } } END { if (!n) print f ": no LOAD segment from " r; exit bad || !n }' \
				|| exit 1; \
			echo "$$so: 16 KB aligned"; \
		done; \
	done

## fmt: format the whole workspace
fmt:
	$(CARGO) fmt --all

## fmt-check: fail if anything is unformatted
fmt-check:
	$(CARGO) fmt --all --check

## clippy: lint everything, warnings are errors
#
# The desktop app three times: without its drills, as it ships, with them, and without its
# default feature (VTE's termprops), as it builds on macOS 12.
clippy:
	$(CARGO) clippy --workspace --all-targets --locked --features accent-core/pdf -- -D warnings
	$(CARGO) clippy -p accent --all-targets --locked --features bench -- -D warnings
	$(CARGO) clippy -p accent --no-default-features --all-targets --locked -- -D warnings

## check: the pre-flight gate, what CI runs
#
# The PDF tests run only when libpdfium is already there, so a fresh clone is not forced into a
# 7 MB download by the gate; `make pdfium` or `make test-pdf` fetches it, and CI does both.
# The skip is loud on purpose: the tests skip themselves silently without the library.
check: fmt-check clippy test drills
	@if test -f $(PDFIUM_LIB); then $(MAKE) test-pdf; else \
		echo "SKIPPED the PDF tests: no $(PDFIUM_LIB). Run \`make pdfium\` (or \`make test-pdf\`) to fetch it."; \
	fi

## doc: build and open the API documentation
doc:
	$(CARGO) doc --workspace --no-deps --open

## docs: serve the mdBook in docs/ and open it (needs mdbook)
docs:
	mdbook serve docs --open

## run: build and run the desktop app on the test vault
run: gtk vault
	$(TARGET_DIR)/accent $(VAULT)

## vault: generate the dummy vault used by tests and benchmarks (regenerated when the generator changes)
#
# A plain prerequisite, not order-only: the fixtures are only as good as the generator that wrote
# them, so an edit to it makes what is on disk stale. `--force` is what lets the recipe write over
# the directory it just found out of date.
vault: $(VAULT)
$(VAULT): $(GEN_VAULT)
	$(CARGO) run $(CARGO_PROFILE_FLAG) -p accent-core --example gen-vault -- \
		$(VAULT) --notes $(VAULT_NOTES) --files $(VAULT_FILES) --force

## xvfb: start the X server the headless checks draw on, unless it is already up
xvfb:
	@command -v Xvfb >/dev/null || { echo "Xvfb is not installed"; exit 1; }
	@# The pid in the display's lock file, not its socket, which a server that died uncleanly
	@# leaves behind, nor `pgrep -f "Xvfb :N"`, which matches the shell running it.
	@kill -0 $$(cat /tmp/.X$(DISPLAY_NUM)-lock 2>/dev/null) 2>/dev/null || \
		{ (Xvfb :$(DISPLAY_NUM) -screen 0 1400x900x24 >/dev/null 2>&1 &); sleep 2; }

## smoke: headless start-up check, fails on any GTK critical
smoke: gtk-bench vault xvfb
	build-aux/headless.sh ACCENT_BENCH_SWITCHER=meeting timeout 60 $(TARGET_DIR)/accent $(VAULT)

## drills: run the drills that assert under Xvfb, each against what it has to print (build-aux/drills.sh)
drills: gtk-bench xvfb
	build-aux/drills.sh $(TARGET_DIR)/accent $(PDFIUM_LIB)

## screenshots: regenerate docs/screenshot-1..5.png and data/screenshots (needs magick, htop, Xvfb, debugpy)
#
# On a vault and repository of their own, made afresh each run; build-aux/screenshots.sh says how.
screenshots: core gtk-bench pdfium
	rm -rf target/screenshots
	$(MAKE) vault VAULT=target/screenshots/testvault
	build-aux/screenshots.sh $(TARGET_DIR)

## server: build the static accent-cli that gets uploaded to a remote host, for x86_64 and aarch64
#
# Static because the host may be older than this machine: a binary linked against Tumbleweed's
# glibc will not start on a stable distro, and the whole point is that any Linux host works.
# rusqlite is bundled C, so this needs a C compiler for each musl target, not only the Rust
# target: `cargo zigbuild` brings one for both and needs no root.
server:
	@command -v cargo-zigbuild >/dev/null && command -v zig >/dev/null || { \
		echo "make server needs cargo-zigbuild and zig (no root needed):"; \
		echo "    uv tool install cargo-zigbuild && ln -s \$$HOME/.local/share/uv/tools/cargo-zigbuild/bin/python-zig \$$HOME/.local/bin/zig"; \
		exit 1; }
	rustup target add $(SERVER_ARCHES:%=%-unknown-linux-musl)
	@for arch in $(SERVER_ARCHES); do \
		$(CARGO) zigbuild -p accent-cli $(CARGO_PROFILE_FLAG) --target "$$arch-unknown-linux-musl" \
			|| exit 1; \
		python3 build-aux/compress-server.py "$(call SERVER_BIN,$$arch)" || exit 1; \
		echo "Server binary: $(call SERVER_BIN,$$arch)"; \
	done

# macOS's app icon, from the SVG: iconutil reads a folder named .iconset with each size and its @2x.
$(TARGET_DIR)/accent.icns: data/icons/hicolor/scalable/apps/$(APP_ID).svg
	iconset=$$(mktemp -d)/accent.iconset && mkdir $$iconset && \
	for s in 16 32 128 256 512; do \
		rsvg-convert -w $$s -h $$s -o $$iconset/icon_$${s}x$$s.png $< && \
		rsvg-convert -w $$((s * 2)) -h $$((s * 2)) -o $$iconset/icon_$${s}x$$s@2x.png $< || exit 1; \
	done && \
	mkdir -p $(dir $@) && iconutil -c icns -o $@ $$iconset && rm -rf $$(dirname $$iconset)

## app: build Accent.app in the target folder, holding every library and file it loads (macOS)
app: all pdfium $(TARGET_DIR)/accent.icns
	build-aux/macos-bundle.sh $(TARGET_DIR) $(VERSION)

## dmg: Accent.app as a disk image to ship, with a link to Applications to drag it onto (macOS)
dmg: app
	rm -rf $(TARGET_DIR)/dmg && mkdir $(TARGET_DIR)/dmg
	ditto $(TARGET_DIR)/Accent.app $(TARGET_DIR)/dmg/Accent.app
	ln -s /Applications $(TARGET_DIR)/dmg/Applications
	hdiutil create -volname Accent -srcfolder $(TARGET_DIR)/dmg -ov -format ULFO \
		$(TARGET_DIR)/accent-$(VERSION)-macos-$(shell uname -m).dmg
	rm -rf $(TARGET_DIR)/dmg

## release: set the version to TAG=vX.Y.Z, commit it, tag it (NOTES="summary" optional) and push both, which starts the Release workflow
release: VER = $(TAG:v%=%)
release:
	@echo '$(TAG)' | grep -Eqx 'v[0-9]+\.[0-9]+\.[0-9]+' || { echo 'usage: make release TAG=vX.Y.Z'; exit 1; }
	@# ponytail: GNU sed; macOS's would need -i '' and a literal newline.
	sed -i 's/^version = ".*"/version = "$(VER)"/' Cargo.toml
	$(CARGO) update -w
	@# The Flatpak builds offline from this list, so a crate added since it was last made fails it.
	$(MAKE) cargo-sources
	sed -i 's|^\( *\)<releases>$$|&\n\1  <release version="$(VER)" date="'$$(date +%F)'"/>|' data/$(APP_ID).metainfo.xml
	git commit -m "release: version $(VER)" Cargo.toml Cargo.lock build-aux/cargo-sources.json data/$(APP_ID).metainfo.xml
	git tag -a $(TAG) -m "accent $(VER)" $(if $(NOTES),-m "$(NOTES)")
	git push origin HEAD $(TAG)

# An installed binary's symbols, in a file named by its build id under $(LIBDIR)/debug: where gdb
# and coredumpctl look for them under /usr, and what a dbgsym or debuginfo package ships.
DEBUG_FILE = $(DESTDIR)$(LIBDIR)/debug/.build-id/$$(readelf -n $(1) | sed -n 's|.*Build ID: \(..\)|\1/|p').debug
# Installs the build $(1) as $(2) stripped, its symbols kept in its DEBUG_FILE, while target/ keeps
# the build whole. The copy it replaces takes its symbols along, so a reinstall leaves none behind.
INSTALL_STRIPPED = { test ! -f $(2) || rm -f $(call DEBUG_FILE,$(2)); } && install -Dm755 $(1) $(2) && \
	debug=$(call DEBUG_FILE,$(2)) && mkdir -p $$(dirname $$debug) && \
	objcopy --only-keep-debug $(2) $$debug && chmod 644 $$debug && strip --strip-unneeded $(2)

## install: install into ~/.local (no sudo), on macOS Accent.app into ~/Applications; override PREFIX for a system-wide install
install: $(if $(DARWIN),app,all)
ifdef DARWIN
	@# The app, and `accent` and `accent-cli` in $(BINDIR) starting its own, for a launch from a
	@# shell and an MCP client's config: scripts rather than links, so that the binary still runs
	@# from inside the app (`platform/bundle.rs`).
	mkdir -p $(DESTDIR)$(APPSDIR) $(DESTDIR)$(BINDIR)
	rm -rf $(DESTDIR)$(APPSDIR)/Accent.app
	ditto $(TARGET_DIR)/Accent.app $(DESTDIR)$(APPSDIR)/Accent.app
	for bin in accent accent-cli; do \
		printf '#!/bin/sh\nexec "%s" "$$@"\n' '$(APPSDIR)/Accent.app/Contents/MacOS/'$$bin \
			>$(DESTDIR)$(BINDIR)/$$bin && chmod 755 $(DESTDIR)$(BINDIR)/$$bin || exit 1; \
	done
else
	$(call INSTALL_STRIPPED,$(TARGET_DIR)/accent,$(DESTDIR)$(BINDIR)/accent)
	$(call INSTALL_STRIPPED,$(TARGET_DIR)/accent-cli,$(DESTDIR)$(BINDIR)/accent-cli)
	install -Dm644 data/$(APP_ID).desktop $(DESTDIR)$(DATADIR)/applications/$(APP_ID).desktop
	install -Dm644 data/$(APP_ID).metainfo.xml $(DESTDIR)$(DATADIR)/metainfo/$(APP_ID).metainfo.xml
	install -Dm644 data/icons/hicolor/scalable/apps/$(APP_ID).svg \
		$(DESTDIR)$(DATADIR)/icons/hicolor/scalable/apps/$(APP_ID).svg
	install -Dm644 data/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg \
		$(DESTDIR)$(DATADIR)/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg
	@# Best effort: `make pdfium` may never have run, and the PDF tab degrades to a status page
	@# without the library. $(LIBDIR)/accent is `../lib/accent` seen from the binary in $(BINDIR),
	@# which is the second place crates/core/src/pdf.rs looks.
	-@test -f $(PDFIUM_LIB) && \
		$(call INSTALL_STRIPPED,$(PDFIUM_LIB),$(DESTDIR)$(LIBDIR)/accent/$(notdir $(PDFIUM_LIB)))
	@# Also best effort: `make server` needs a musl toolchain, and everything but opening a vault
	@# on another machine works without it. Same `../lib/accent` the app looks in for libpdfium.
	-@for arch in $(SERVER_ARCHES); do \
		test -f $(call SERVER_BIN,$$arch) || continue; \
		install -Dm755 $(call SERVER_BIN,$$arch) $(DESTDIR)$(LIBDIR)/accent/accent-cli-$$arch; \
		for suffix in .xz .xz.sha256; do \
			test ! -f $(call SERVER_BIN,$$arch)$$suffix || \
				install -m644 $(call SERVER_BIN,$$arch)$$suffix $(DESTDIR)$(LIBDIR)/accent/accent-cli-$$arch$$suffix; \
		done; \
	done; true
	@# Best effort: without these the launcher and icon can take a re-login to appear. Never into
	@# a DESTDIR, whose caches would end up in a package and clash with the system's own.
ifndef DESTDIR
	-@update-desktop-database $(DATADIR)/applications 2>/dev/null
	-@gtk-update-icon-cache -qtf $(DATADIR)/icons/hicolor 2>/dev/null
endif
endif
	@echo "Installed to $(DESTDIR)$(if $(DARWIN),$(APPSDIR)/Accent.app,$(PREFIX))"
	@case ":$$PATH:" in *":$(BINDIR):"*) ;; \
		*) echo "Note: $(BINDIR) is not in your PATH." ;; esac

## uninstall: remove what install put in place
uninstall:
ifndef DARWIN
	@# The symbols first, found by the build ids of the binaries still in place.
	-@for f in $(DESTDIR)$(BINDIR)/accent $(DESTDIR)$(BINDIR)/accent-cli \
		$(DESTDIR)$(LIBDIR)/accent/$(notdir $(PDFIUM_LIB)); do \
		test ! -f $$f || rm -f $(call DEBUG_FILE,$$f); \
	done; rmdir $(DESTDIR)$(LIBDIR)/debug/.build-id/* $(DESTDIR)$(LIBDIR)/debug/.build-id \
		$(DESTDIR)$(LIBDIR)/debug 2>/dev/null; true
endif
	rm -f $(DESTDIR)$(BINDIR)/accent $(DESTDIR)$(BINDIR)/accent-cli \
		$(DESTDIR)$(DATADIR)/applications/$(APP_ID).desktop \
		$(DESTDIR)$(DATADIR)/metainfo/$(APP_ID).metainfo.xml \
		$(DESTDIR)$(DATADIR)/icons/hicolor/scalable/apps/$(APP_ID).svg \
		$(DESTDIR)$(DATADIR)/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg \
		$(DESTDIR)$(LIBDIR)/accent/$(notdir $(PDFIUM_LIB)) $(DESTDIR)$(LIBDIR)/accent/accent-cli-*
	-@rmdir $(DESTDIR)$(LIBDIR)/accent 2>/dev/null
ifdef DARWIN
	rm -rf $(DESTDIR)$(APPSDIR)/Accent.app
endif
	@# Only where install refreshed it: never a DESTDIR's, and macOS has none.
ifeq ($(DARWIN)$(DESTDIR),)
	-@update-desktop-database $(DATADIR)/applications 2>/dev/null
endif
	@echo "Uninstalled from $(DESTDIR)$(PREFIX)"

# What a .deb or .rpm packs: the files `install` puts under /usr, with libpdfium and both servers
# required rather than best effort. `build-aux/native-package.sh` builds either one in a container
# of the distribution it is for.
PKGROOT = $(abspath $(TARGET_DIR))/pkgroot
pkgroot: all pdfium
	@for arch in $(SERVER_ARCHES); do test -f $(call SERVER_BIN,$$arch).xz || \
		{ echo "missing $(call SERVER_BIN,$$arch).xz: run make server first"; exit 1; }; done
	rm -rf $(PKGROOT) $(PKGROOT)-debug
	$(MAKE) --no-print-directory install DESTDIR=$(PKGROOT) PREFIX=/usr
	@# The symbols `install` split off stay out of the package, kept beside it for a debug one.
	mkdir -p $(PKGROOT)-debug/usr/lib && mv $(PKGROOT)/usr/lib/debug $(PKGROOT)-debug/usr/lib/

## deb: package the app as a .deb in the target folder, for the Debian or Ubuntu it is built on
#
# dpkg-shlibdeps names the package behind each library the binaries link, reading the version
# floors from this system's symbol files. It wants a source tree's debian/control, so it gets an
# empty one of its own. The servers are static and need nothing.
deb: pkgroot
	install -Dm644 LICENSE $(PKGROOT)/usr/share/doc/accent/copyright
	rm -rf $(TARGET_DIR)/deb && mkdir -p $(TARGET_DIR)/deb/debian $(PKGROOT)/DEBIAN
	touch $(TARGET_DIR)/deb/debian/control
	cd $(TARGET_DIR)/deb && dpkg-shlibdeps -O $(PKGROOT)/usr/bin/accent $(PKGROOT)/usr/bin/accent-cli \
		$(PKGROOT)/usr/lib/accent/libpdfium.so | sed 's/^shlibs:Depends=//' > depends
	@# glib-networking is what gives libsoup HTTPS, which no linked library names.
	printf '%s\n' "Package: accent" "Version: $(VERSION)" "Architecture: $$(dpkg --print-architecture)" \
		"Maintainer: Melvin Strobl <stroblme@posteo.de>" "Depends: $$(cat $(TARGET_DIR)/deb/depends), glib-networking" \
		"Recommends: git, openssh-client" "Section: editors" "Priority: optional" \
		"Homepage: https://github.com/stroblme/accent" "Description: Markdown and PDF knowledge editor" \
		> $(PKGROOT)/DEBIAN/control
	dpkg-deb --root-owner-group --build $(PKGROOT) $(TARGET_DIR)/accent_$(VERSION)_$$(dpkg --print-architecture).deb

## rpm: package the app as an .rpm in the target folder, for the Fedora or openSUSE it is built on
rpm: pkgroot
	rpmbuild -bb --build-in-place --define "_topdir $(abspath $(TARGET_DIR))/rpm" \
		--define "ver $(VERSION)" --define "pkgroot $(PKGROOT)" build-aux/accent.spec

## validate: check the desktop file and the AppStream metainfo
validate:
	desktop-file-validate data/$(APP_ID).desktop
	appstreamcli validate --no-net data/$(APP_ID).metainfo.xml

## icons: regenerate the installed icons from data/icons/logo.svg (needs inkscape)
icons:
	python3 build-aux/derive-icons.py

## cargo-sources: regenerate the Flatpak vendored-source list (needed whenever Cargo.lock changes)
cargo-sources:
	uv run --with aiohttp,pyyaml,tomlkit \
		https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py \
		Cargo.lock -o build-aux/cargo-sources.json

## flatpak: build and install the Flatpak (needs org.flatpak.Builder and the GNOME 51 SDK)
flatpak:
	flatpak run org.flatpak.Builder --user --install --force-clean \
		build-aux/build build-aux/$(APP_ID).yml

## clean: remove build artifacts
clean:
	$(CARGO) clean
	rm -rf build-aux/build .flatpak-builder

## distclean: also remove the generated test vault and the index caches it left behind
distclean: clean
	rm -rf $(VAULT) $(VAULT)-external

## help: list the targets
help:
	@echo "accent targets (override PREFIX, DESTDIR, PROFILE, VAULT as needed):"
	@sed -n 's/^## //p' $(MAKEFILE_LIST) | awk -F': ' '{printf "  \033[1m%-14s\033[0m %s\n", $$1, $$2}'
