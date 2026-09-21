{
  description = "Note: self-hosted daily planning server";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.crane.url = "github:ipetkov/crane";
  outputs = { self, nixpkgs, crane }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      inherit (pkgs) lib;
      craneLib = crane.mkLib pkgs;
      version = (builtins.fromTOML (builtins.readFile ./server/Cargo.toml)).package.version;

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
        '';
        passthru.web = web;
      });
    in {
      packages.${system} = {
        default = server;
        note-server = server;
        note-web = web;
      };
      checks.${system}.note-server-tests = craneLib.cargoTest (commonArgs // {
        inherit cargoArtifacts;
      });
      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [ cargo rustc rustfmt clippy nodejs pnpm sqlite ];
      };
    };
}
