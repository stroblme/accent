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

# libpdfium is a 7 MB binary, so it is not in git: `make pdfium` fetches the matching build from
# bblanchon/pdfium-binaries. Bump PDFIUM_BUILD and every checksum below it together; the release
# publishes one per asset.
PDFIUM_BUILD  := 8035
PDFIUM_RELEASE := https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F$(PDFIUM_BUILD)
# The desktop build follows the machine rather than assuming x86-64.
PDFIUM_ARCH   := $(if $(filter aarch64,$(shell uname -m)),arm64,x64)
PDFIUM_URL    := $(PDFIUM_RELEASE)/pdfium-linux-$(PDFIUM_ARCH).tgz
PDFIUM_SHA256_x64   := 2e6db042dd2cff2d5247023dbec6c7ebb800042ce83c835d6468d45229669bd4
PDFIUM_SHA256_arm64 :=
PDFIUM_SHA256 := $(PDFIUM_SHA256_$(PDFIUM_ARCH))
PDFIUM_LIB    := vendor/pdfium/libpdfium.so

# Android. The two ABIs the APK ships: the phone, and the emulator this machine can run.
# `jniLibs/<abi>` is where Gradle packs a bare `.so` into the APK, and where Android's linker
# then finds it by name — which is how `Pdfium::bind_to_system_library` gets hold of it.
ANDROID_ABIS  := arm64-v8a x86_64
JNI_LIBS      := android/app/src/main/jniLibs
PDFIUM_SHA256_android_arm64-v8a := 22280f42b38dc86919c93988d5cef8e39e0ab6d880a7b953531ed9775724bcdf
PDFIUM_SHA256_android_x86_64    := 67a1865b961e9c58d1ada607e8b40c685685cc34de29aa6242c0ba588f4691ff
# `jniLibs` is named by ABI, the pdfium release by architecture.
PDFIUM_ASSET_arm64-v8a := arm64
PDFIUM_ASSET_x86_64    := x64
ANDROID_TARGETS := aarch64-linux-android x86_64-linux-android
# A release cross build takes `[profile.android]` from Cargo.toml — release plus `opt-level = "z"`
# and `strip` — because the `.so` ships in the APK and the desktop must keep release's speed.
ANDROID_PROFILE_FLAG := $(if $(filter release,$(PROFILE)),--profile android,)
# uniffi keeps its metadata in the symbol table, which that profile strips, so the bindings are
# read out of a host build of the same crate — the flow uniffi documents. Dev profile, because
# that is the one the generator itself is built with.
HOST_FFI_LIB := $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),target)/debug/libaccent_android.so
# The NDK's own readelf, for the 16 KB page-size check; the host's would do, but the NDK is what
# a machine building for Android is guaranteed to have.
ANDROID_NDK_HOME ?= $(firstword $(wildcard $(HOME)/Android/Sdk/ndk/*))
READELF ?= $(ANDROID_NDK_HOME)/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-readelf

# The remote server is this same CLI, built static so it starts on a host older than this one.
# Its name on the remote is its own blake3, so a rebuild re-provisions exactly once.
MUSL_TARGET := x86_64-unknown-linux-musl
SERVER_BIN  := $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),target)/$(MUSL_TARGET)/$(PROFILE)/accent-cli

# `cargo build` puts a release build under target/release and a dev build under target/debug.
CARGO_PROFILE_FLAG := $(if $(filter release,$(PROFILE)),--release,)
# Honours CARGO_TARGET_DIR, which parallel worktrees must each set to a directory of their
# own (ROADMAP §6): without this `smoke` and `install` look for a binary cargo never wrote there.
TARGET_DIR := $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),target)/$(PROFILE)

# The vault used by every test and benchmark. Never point these at a real vault.
VAULT     ?= testvault
VAULT_NOTES ?= 3600
VAULT_FILES ?= 40000
# Repo tooling rather than a CLI subcommand, so the fixtures are not a public feature of the app.
GEN_VAULT := crates/core/examples/gen-vault.rs

# Headless runs need an X server; :99 is what ROADMAP.md and CI use. GDK_BACKEND=x11 because
# DISPLAY alone leaves GTK on a Wayland session; cairo because there is no GL under Xvfb; no a11y
# because the private bus has no registry; fatal-criticals so a GTK critical fails the check.
DISPLAY_NUM ?= 99
XVFB_ENV := DISPLAY=:$(DISPLAY_NUM) GDK_BACKEND=x11 GSK_RENDERER=cairo GTK_A11Y=none G_DEBUG=fatal-criticals

.DEFAULT_GOAL := all
.PHONY: all core gtk clean distclean install uninstall test test-pdf check fmt fmt-check \
        clippy doc run smoke vault validate icons flatpak cargo-sources pdfium server help \
        android android-check android-test android-tools apk apk-release pdfium-android bindings

## all: build everything, core plus the desktop app
all: core gtk

## core: build accent-core, accent-api and accent-cli
core:
	$(CARGO) build $(CARGO_PROFILE_FLAG)

## gtk: build the desktop app (needs gtk4, libadwaita, gtksourceview5, webkitgtk-6.0, libspelling, vte-2.91-gtk4 dev packages)
gtk:
	$(CARGO) build $(CARGO_PROFILE_FLAG) -p accent

## test: run the whole workspace test suite, desktop app included
test:
	$(CARGO) test --workspace --locked

## test-pdf: run the PDF tests too (needs libpdfium, see vendor/pdfium or ACCENT_PDFIUM_DIR)
test-pdf: pdfium
	$(CARGO) test -p accent-core -p accent-api --features pdf --locked

## pdfium: fetch libpdfium into vendor/pdfium (does nothing if it is already there)
pdfium: | $(PDFIUM_LIB)
$(PDFIUM_LIB):
	@test -n "$(PDFIUM_SHA256)" || \
		{ echo "PDFIUM_SHA256 is empty: refusing to install an unverified libpdfium"; exit 1; }
	@mkdir -p $(dir $(PDFIUM_LIB))
	curl -fL --retry 3 -o $(PDFIUM_LIB).tgz "$(PDFIUM_URL)"
	echo "$(PDFIUM_SHA256)  $(PDFIUM_LIB).tgz" | sha256sum -c -
	@# The release keeps the library under lib/ and its metadata at the top level; we want the
	@# three files side by side, and none of the headers.
	tar -xzf $(PDFIUM_LIB).tgz -C $(dir $(PDFIUM_LIB)) --strip-components=1 lib/libpdfium.so
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
pdfium-android: $(foreach abi,$(ANDROID_ABIS),$(JNI_LIBS)/$(abi)/libpdfium.so)

# One rule per ABI: the checksum and the asset name both depend on which one it is, and a static
# pattern rule cannot reach either.
define pdfium-android-rule
$(JNI_LIBS)/$(1)/libpdfium.so:
	@test -n "$(PDFIUM_SHA256_android_$(1))" || \
		{ echo "no sha256 for pdfium-android-$(PDFIUM_ASSET_$(1)): refusing to install it unverified"; exit 1; }
	@mkdir -p $(JNI_LIBS)/$(1)
	curl -fL --retry 3 -o $(JNI_LIBS)/$(1)/pdfium.tgz \
		"$(PDFIUM_RELEASE)/pdfium-android-$(PDFIUM_ASSET_$(1)).tgz"
	echo "$(PDFIUM_SHA256_android_$(1))  $(JNI_LIBS)/$(1)/pdfium.tgz" | sha256sum -c -
	tar -xzf $(JNI_LIBS)/$(1)/pdfium.tgz -C $(JNI_LIBS)/$(1) --strip-components=1 lib/libpdfium.so
	rm -f $(JNI_LIBS)/$(1)/pdfium.tgz
endef
$(foreach abi,$(ANDROID_ABIS),$(eval $(call pdfium-android-rule,$(abi))))

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
	@# load there at all. Every LOAD segment must be aligned to 0x4000.
	@for abi in $(ANDROID_ABIS); do \
		for so in $(JNI_LIBS)/$$abi/*.so; do \
			test -f "$$so" || continue; \
			$(READELF) -lW "$$so" | awk -v f="$$so" '/LOAD/ { if ($$NF != "0x4000") { print f " is not 16 KB aligned: " $$NF; bad=1 } } END { exit bad }' \
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
clippy:
	$(CARGO) clippy --workspace --all-targets --locked --features accent-core/pdf -- -D warnings

## check: the pre-flight gate, what CI runs
#
# The PDF tests run only when libpdfium is already there, so a fresh clone is not forced into a
# 7 MB download by the gate; `make pdfium` or `make test-pdf` fetches it, and CI does both.
# The skip is loud on purpose: the tests skip themselves silently without the library.
check: fmt-check clippy test
	@if test -f $(PDFIUM_LIB); then $(MAKE) test-pdf; else \
		echo "SKIPPED the PDF tests: no $(PDFIUM_LIB). Run \`make pdfium\` (or \`make test-pdf\`) to fetch it."; \
	fi

## doc: build and open the API documentation
doc:
	$(CARGO) doc --workspace --no-deps --open

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

## smoke: headless start-up check, fails on any GTK critical
smoke: gtk vault
	@command -v Xvfb >/dev/null || { echo "Xvfb is not installed"; exit 1; }
	@# The display's socket, not `pgrep -f "Xvfb :N"`: that pattern matches the shell running it.
	@test -S /tmp/.X11-unix/X$(DISPLAY_NUM) || (Xvfb :$(DISPLAY_NUM) -screen 0 1400x900x24 >/dev/null 2>&1 &)
	@sleep 2
	@# A private session bus per run: accent is a single-instance GApplication, so without one a
	@# second invocation forwards its arguments to whatever instance is already up and exits 0,
	@# which makes this check pass while proving nothing.
	@# The XDG dirs point at a scratch directory, set outside the bus so whatever it activates sees
	@# them too: a check must neither read the user's config nor write a session and an index.
	xdg=$$(mktemp -d) && trap 'rm -rf "$$xdg"' EXIT && \
	env XDG_CONFIG_HOME="$$xdg/config" XDG_CACHE_HOME="$$xdg/cache" \
		XDG_STATE_HOME="$$xdg/state" XDG_DATA_HOME="$$xdg/data" \
	dbus-run-session -- env $(XVFB_ENV) ACCENT_BENCH_SWITCHER=meeting timeout 60 $(TARGET_DIR)/accent $(VAULT)

## server: build the static accent-cli that gets uploaded to a remote host
#
# Static because the host may be older than this machine: a binary linked against Tumbleweed's
# glibc will not start on a stable distro, and the whole point is that any x86_64 Linux works.
# rusqlite is bundled C, so this needs a musl-capable C compiler, not only the Rust target.
# `cargo zigbuild` brings its own and needs no root, which is why it is tried first.
server:
	@rustup target list --installed | grep -qx '$(MUSL_TARGET)' \
		|| rustup target add $(MUSL_TARGET)
	@if command -v cargo-zigbuild >/dev/null && command -v zig >/dev/null; then \
		$(CARGO) zigbuild -p accent-cli $(CARGO_PROFILE_FLAG) --target $(MUSL_TARGET); \
	elif command -v musl-gcc >/dev/null || command -v x86_64-linux-musl-gcc >/dev/null; then \
		$(CARGO) build -p accent-cli $(CARGO_PROFILE_FLAG) --target $(MUSL_TARGET); \
	else \
		echo "No musl C toolchain. Either (no root needed):"; \
		echo "    uv tool install cargo-zigbuild && ln -s \$$HOME/.local/share/uv/tools/cargo-zigbuild/bin/python-zig \$$HOME/.local/bin/zig"; \
		echo "or install your distro's musl package (openSUSE: musl-devel, Debian: musl-tools)."; \
		exit 1; \
	fi
	@echo "Server binary: $(SERVER_BIN)"

## install: install into ~/.local (no sudo); override PREFIX for a system-wide install
install: all
	install -Dm755 $(TARGET_DIR)/accent      $(DESTDIR)$(BINDIR)/accent
	install -Dm755 $(TARGET_DIR)/accent-cli  $(DESTDIR)$(BINDIR)/accent-cli
	install -Dm644 data/$(APP_ID).desktop \
		$(DESTDIR)$(DATADIR)/applications/$(APP_ID).desktop
	install -Dm644 data/$(APP_ID).metainfo.xml \
		$(DESTDIR)$(DATADIR)/metainfo/$(APP_ID).metainfo.xml
	install -Dm644 data/icons/hicolor/scalable/apps/$(APP_ID).svg \
		$(DESTDIR)$(DATADIR)/icons/hicolor/scalable/apps/$(APP_ID).svg
	install -Dm644 data/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg \
		$(DESTDIR)$(DATADIR)/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg
	@# Best effort: `make pdfium` may never have run, and the PDF tab degrades to a status page
	@# without the library. $(LIBDIR)/accent is `../lib/accent` seen from the binary in $(BINDIR),
	@# which is the second place crates/core/src/pdf.rs looks.
	-@test -f $(PDFIUM_LIB) && install -Dm755 $(PDFIUM_LIB) $(DESTDIR)$(LIBDIR)/accent/libpdfium.so
	@# Also best effort: `make server` needs a musl toolchain, and everything but opening a vault
	@# on another machine works without it. Same `../lib/accent` the app looks in for libpdfium.
	-@test -f $(SERVER_BIN) && install -Dm755 $(SERVER_BIN) $(DESTDIR)$(LIBDIR)/accent/accent-cli
	@# Best effort: without these the launcher and icon can take a re-login to appear.
	-@update-desktop-database $(DESTDIR)$(DATADIR)/applications 2>/dev/null
	-@gtk-update-icon-cache -qtf $(DESTDIR)$(DATADIR)/icons/hicolor 2>/dev/null
	@echo "Installed to $(DESTDIR)$(PREFIX)"
	@case ":$$PATH:" in *":$(BINDIR):"*) ;; \
		*) echo "Note: $(BINDIR) is not in your PATH." ;; esac

## uninstall: remove what install put in place
uninstall:
	rm -f $(DESTDIR)$(BINDIR)/accent $(DESTDIR)$(BINDIR)/accent-cli
	rm -f $(DESTDIR)$(DATADIR)/applications/$(APP_ID).desktop
	rm -f $(DESTDIR)$(DATADIR)/metainfo/$(APP_ID).metainfo.xml
	rm -f $(DESTDIR)$(DATADIR)/icons/hicolor/scalable/apps/$(APP_ID).svg
	rm -f $(DESTDIR)$(DATADIR)/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg
	rm -f $(DESTDIR)$(LIBDIR)/accent/libpdfium.so
	-@rmdir $(DESTDIR)$(LIBDIR)/accent 2>/dev/null
	-@update-desktop-database $(DESTDIR)$(DATADIR)/applications 2>/dev/null
	@echo "Uninstalled from $(DESTDIR)$(PREFIX)"

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

## flatpak: build and install the Flatpak (needs org.flatpak.Builder and the GNOME 50 SDK)
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
