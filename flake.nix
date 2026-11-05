{
  description = "Markdown and PDF knowledge editor for GNOME";

  # nixos-unstable, although nixos-26.05's Rust (1.95) meets the floor: its pdfium (7749) lacks
  # functions pdfium-render binds for pdfium 7881, so the PDF tab would not load there.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
      ];
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          inherit (pkgs) lib;
        in
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "accent";
            version = (lib.importTOML ./Cargo.toml).workspace.package.version;

            # Only what cargo reads and the install below copies, so `target/` and the test
            # vaults never reach the store. Cargo loads every workspace member, hence android/ffi.
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./.cargo
                ./Cargo.toml
                ./Cargo.lock
                ./crates
                ./apps
                ./android/ffi
                ./data
              ];
            };
            cargoLock.lockFile = ./Cargo.lock;

            # The GTK app is not a default workspace member, so both binaries are named.
            cargoBuildFlags = [
              "-p"
              "accent"
              "-p"
              "accent-cli"
            ];
            # The test suite needs Xvfb and a generated vault.
            doCheck = false;

            nativeBuildInputs = with pkgs; [
              pkg-config
              glib # glib-compile-resources
              wrapGAppsHook4
            ];
            buildInputs = with pkgs; [
              gtk4
              libadwaita
              gtksourceview5
              harfbuzz
              libsoup_3
              vte-gtk4
              libspelling
              glib-networking # HTTPS for libsoup
            ];

            # The layout `make install` gives, minus the static `accent-cli-<arch>` servers that
            # `make server` builds, so this build cannot open a vault on another machine.
            postInstall = ''
              install -Dm644 data/io.github.stroblme.Accent.desktop -t $out/share/applications
              install -Dm644 data/io.github.stroblme.Accent.metainfo.xml -t $out/share/metainfo
              install -Dm644 data/icons/hicolor/scalable/apps/io.github.stroblme.Accent.svg \
                -t $out/share/icons/hicolor/scalable/apps
              install -Dm644 data/icons/hicolor/symbolic/apps/io.github.stroblme.Accent-symbolic.svg \
                -t $out/share/icons/hicolor/symbolic/apps
              # The app looks for libpdfium in `../lib/accent` beside its binary.
              mkdir -p $out/lib/accent
              ln -s ${pkgs.pdfium}/lib/libpdfium.so $out/lib/accent/libpdfium.so
            '';

            meta = {
              description = "Markdown and PDF knowledge editor for GNOME";
              homepage = "https://github.com/stroblme/accent";
              license = lib.licenses.gpl3Plus;
              mainProgram = "accent";
              platforms = lib.platforms.linux;
            };
          };
        }
      );
    };
}
