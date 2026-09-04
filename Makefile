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
DATADIR   ?= $(PREFIX)/share

# `cargo build` puts a release build under target/release and a dev build under target/debug.
CARGO_PROFILE_FLAG := $(if $(filter release,$(PROFILE)),--release,)
TARGET_DIR := target/$(PROFILE)

# The vault used by every test and benchmark. Never point these at a real vault.
VAULT     ?= testvault
VAULT_NOTES ?= 3600
VAULT_FILES ?= 40000

# Headless runs need an X server; :99 is what ROADMAP.md and CI use.
DISPLAY_NUM ?= 99
XVFB_ENV := DISPLAY=:$(DISPLAY_NUM) GSK_RENDERER=cairo GTK_A11Y=none G_DEBUG=fatal-criticals

.DEFAULT_GOAL := all
.PHONY: all core gtk clean distclean install uninstall test test-pdf check fmt fmt-check \
        clippy doc run smoke vault validate icons flatpak cargo-sources help

## all: build everything, core plus the desktop app
all: core gtk

## core: build accent-core, accent-api and accent-cli
core:
	$(CARGO) build $(CARGO_PROFILE_FLAG)

## gtk: build the desktop app (needs gtk4, libadwaita, gtksourceview5, webkitgtk-6.0, libspelling dev packages)
gtk:
	$(CARGO) build $(CARGO_PROFILE_FLAG) -p accent

## test: run the whole workspace test suite, desktop app included
test:
	$(CARGO) test --workspace --locked

## test-pdf: run the PDF tests too (needs libpdfium, see vendor/pdfium or ACCENT_PDFIUM_DIR)
test-pdf:
	$(CARGO) test -p accent-core --features pdf --locked

## fmt: format the whole workspace
fmt:
	$(CARGO) fmt --all

## fmt-check: fail if anything is unformatted
fmt-check:
	$(CARGO) fmt --all --check

## clippy: lint everything, warnings are errors
clippy:
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings

## check: the pre-flight gate, what CI runs
check: fmt-check clippy test

## doc: build and open the API documentation
doc:
	$(CARGO) doc --workspace --no-deps --open

## run: build and run the desktop app on the test vault
run: gtk vault
	$(TARGET_DIR)/accent $(VAULT)

## vault: generate the dummy vault used by tests and benchmarks (skipped if it exists)
vault: | $(VAULT)
$(VAULT):
	$(CARGO) run $(CARGO_PROFILE_FLAG) -p accent-cli -- \
		gen-vault $(VAULT) --notes $(VAULT_NOTES) --files $(VAULT_FILES)

## smoke: headless start-up check, fails on any GTK critical
smoke: gtk vault
	@command -v Xvfb >/dev/null || { echo "Xvfb is not installed"; exit 1; }
	@pgrep -f "Xvfb :$(DISPLAY_NUM)" >/dev/null || (Xvfb :$(DISPLAY_NUM) -screen 0 1400x900x24 >/dev/null 2>&1 &)
	@sleep 2
	@# A private session bus per run: accent is a single-instance GApplication, so without one a
	@# second invocation forwards its arguments to whatever instance is already up and exits 0,
	@# which makes this check pass while proving nothing.
	dbus-run-session -- env $(XVFB_ENV) ACCENT_BENCH_SWITCHER=meeting timeout 60 $(TARGET_DIR)/accent $(VAULT)

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
