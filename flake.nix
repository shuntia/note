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
              hash = "sha256-R8RvENJZiUwHU13A3gCFYPkwB+jbrwWqPin/B5qBNUc=";
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
              fileset = lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./server ];
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

          # The prompt tests read the shipped defaults beside the crate.
          tests = craneLib.cargoTest (commonArgs // {
            inherit cargoArtifacts;
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./server ./config/defaults ];
            };
          });
        in
        { inherit web server tests; };
    in {
      nixosModules.default = import ./nix/module.nix self;

      overlays.default = final: prev: { note-server = (build final).server; };

      packages = forAllSystems (pkgs:
        let b = build pkgs; in {
          default = b.server;
          note-server = b.server;
          note-web = b.web;
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
