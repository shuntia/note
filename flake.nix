{
  description = "Note: self-hosted daily planning server";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      version = (builtins.fromTOML (builtins.readFile ./server/Cargo.toml)).package.version;

      web = pkgs.stdenv.mkDerivation (finalAttrs: {
        pname = "note-web";
        inherit version;
        src = ./web;
        nativeBuildInputs = with pkgs; [ nodejs pnpm pnpmConfigHook ];
        pnpmDeps = pkgs.fetchPnpmDeps {
          inherit (finalAttrs) pname version src;
          fetcherVersion = 4;
          hash = "sha256-0b0x/CNVTBeH0fCWSyTf0pYlTdgGuQGRT71riClxvKM=";
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

      server = pkgs.rustPlatform.buildRustPackage {
        pname = "note-server";
        inherit version;
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;
        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = [ pkgs.openssl ];
        NOTE_DEFAULT_WEB_DIR = "${placeholder "out"}/share/note/web";
        postInstall = ''
          mkdir -p $out/share/note
          ln -s ${web} $out/share/note/web
        '';
        passthru.web = web;
      };
    in {
      packages.${system} = {
        default = server;
        note-server = server;
        note-web = web;
      };
      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [ cargo rustc rustfmt clippy nodejs pnpm sqlite ];
      };
    };
}
