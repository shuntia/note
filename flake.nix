{
  description = "Note: self-hosted daily planning server";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.crane.url = "github:ipetkov/crane";
  outputs = { self, nixpkgs, crane }:
    let
      inherit (nixpkgs) lib;
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      version = (builtins.fromTOML (builtins.readFile ./server/Cargo.toml)).package.version;

      build = pkgs:
        let
          craneLib = crane.mkLib pkgs;

          web = pkgs.stdenv.mkDerivation (finalAttrs: {
            pname = "note-web";
            inherit version;
            src = ./web;
            nativeBuildInputs = with pkgs; [ nodejs pnpm pnpmConfigHook ];
            pnpmDeps = pkgs.fetchPnpmDeps {
              inherit (finalAttrs) pname version src;
              fetcherVersion = 4;
              hash = "sha256-C79vJRSYzRjOrahqwPGefU6mbRjluGIC1EQ9OLzRIsY=";
            };
            buildPhase = ''
              runHook preBuild
              pnpm build
              runHook postBuild
            '';
            installPhase = ''
              runHook preInstall
              cp -r dist $out
              runHook postInstall
            '';
            meta = {
              description = "Web client for the Note daily planning server";
              license = lib.licenses.unlicense;
            };
          });

          # Only the crate itself: edits to config/, docs/ or web/ leave the
          # server's inputs untouched, so they never trigger a Rust rebuild.
          commonArgs = {
            pname = "note-server";
            inherit version;
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./server ./voice-proto ./voice ];
            };
            strictDeps = true;
            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = [ pkgs.openssl ];
          };

          # Keyed on Cargo.lock, so the dependency tree compiles once and is
          # reused by every later build of the crate.
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;

          server = craneLib.buildPackage (commonArgs // {
            inherit cargoArtifacts;
            cargoExtraArgs = "-p note-server";
            doCheck = false;
            NOTE_DEFAULT_WEB_DIR = "${placeholder "out"}/share/note/web";
            postInstall = ''
              mkdir -p $out/share/note
              ln -s ${web} $out/share/note/web
              cp -r --no-preserve=mode ${./config/defaults} $out/share/note/defaults
            '';
            passthru.web = web;
            meta = {
              description = "Self-hosted daily planning server";
              license = lib.licenses.unlicense;
              mainProgram = "note-server";
            };
          });

          voice = craneLib.buildPackage (commonArgs // {
            pname = "note-voice";
            inherit cargoArtifacts;
            cargoExtraArgs = "-p note-voice";
            doCheck = false;
            meta = {
              description = "Rings a linked Matrix account for Note";
              license = lib.licenses.unlicense;
              mainProgram = "note-voice";
            };
          });

          # The prompt tests read the shipped defaults beside the crate; the
          # voice tests build HTTP clients, which refuse to start without CA roots.
          tests = craneLib.cargoTest (commonArgs // {
            inherit cargoArtifacts;
            SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./server ./voice-proto ./voice ./config/defaults ];
            };
          });

          # Loads the live site, so only the shell's own files ship; nixpkgs'
          # electron stands in for the npm binary, which cannot run on NixOS.
          desktop = pkgs.stdenv.mkDerivation {
            pname = "note-desktop";
            version = (lib.importJSON ./desktop/package.json).version;
            src = lib.fileset.toSource {
              root = ./desktop;
              fileset = lib.fileset.unions [ ./desktop/main.js ./desktop/offline.html ./desktop/package.json ./desktop/icons ];
            };
            nativeBuildInputs = with pkgs; [ makeWrapper copyDesktopItems ];
            desktopItems = [
              (pkgs.makeDesktopItem {
                name = "note-desktop";
                desktopName = "Note";
                comment = "Daily planning";
                exec = "note-desktop %U";
                icon = "note-desktop";
                categories = [ "Office" ];
              })
            ];
            installPhase = ''
              runHook preInstall
              mkdir -p $out/share/note-desktop
              cp -r main.js offline.html package.json icons $out/share/note-desktop/
              for f in icons/*x*.png; do
                size=$(basename $f .png)
                install -Dm644 $f $out/share/icons/hicolor/$size/apps/note-desktop.png
              done
              makeWrapper ${pkgs.electron}/bin/electron $out/bin/note-desktop \
                --add-flags $out/share/note-desktop \
                --set NOTE_DESKTOP_EXEC $out/bin/note-desktop
              runHook postInstall
            '';
            meta = {
              description = "Desktop wrapper for the Note daily planning app";
              license = lib.licenses.unlicense;
              mainProgram = "note-desktop";
            };
          };
        in
        { inherit web server voice tests desktop; };
    in {
      nixosModules.default = import ./nix/module.nix self;

      overlays.default = final: prev: {
        note-server = (build final).server;
        note-voice = (build final).voice;
      };

      packages = forAllSystems (pkgs:
        let b = build pkgs; in {
          default = b.server;
          note-server = b.server;
          note-voice = b.voice;
          note-web = b.web;
          note-desktop = b.desktop;
          desktop = b.desktop;
        });

      checks = forAllSystems (pkgs:
        { note-server-tests = (build pkgs).tests; }
        // lib.optionalAttrs (pkgs.stdenv.hostPlatform.system == "x86_64-linux") {
          module = pkgs.testers.runNixOSTest (import ./nix/test.nix self);
        });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [ cargo rustc rustfmt clippy nodejs pnpm sqlite ];
        };
      });
    };
}
