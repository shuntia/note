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
          cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
            cargoExtraArgs = "--locked --workspace --exclude note-voice";
          });

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

          system = pkgs.stdenv.hostPlatform.system;
          pkgsCuda = import nixpkgs {
            inherit system;
            config.allowUnfreePredicate = p:
              lib.hasPrefix "cuda" (lib.getName p)
              || lib.elem (lib.getName p) [ "cudnn" "libcublas" "libcufft" "libcurand" "libnvjitlink" ];
          };
          cuda = pkgsCuda.cudaPackages_12_8;

          webrtc = pkgs.fetchzip {
            url = "https://github.com/livekit/rust-sdks/releases/download/webrtc-89d790b/webrtc-linux-x64-release.zip";
            hash = "sha256-NrLorUlvUNdpI3IXEnJXrRWoMu7IdPdM9vNFp0DTK+M=";
            stripRoot = false;
          };

          # libcuda.so.1 comes from the host driver at run time, and the TensorRT
          # provider is never loaded. The CUDA provider dlopens cuDNN and cuFFT.
          sherpaGpu = pkgs.stdenv.mkDerivation {
            pname = "sherpa-onnx-gpu";
            version = "1.13.8";
            src = pkgs.fetchurl {
              url = "https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.13.8/sherpa-onnx-v1.13.8-cuda-12.x-cudnn-9.x-onnxruntime1.28.2-linux-x64-gpu.tar.bz2";
              hash = "sha256-ITKvGujITIT4asCR5DEYOSh4UdK/XmYDMuvOOJih4LI=";
            };
            nativeBuildInputs = [ pkgs.autoPatchelfHook ];
            buildInputs = with cuda; [ cuda_cudart libcublas libcufft libcurand cudnn ] ++ [ pkgs.stdenv.cc.cc.lib ];
            appendRunpaths = map (p: "${lib.getLib p}/lib") (with cuda; [ cudnn libcufft ]);
            autoPatchelfIgnoreMissingDeps = [ "libcuda.so.1" "libnvinfer.so.10" "libnvinfer_plugin.so.10" "libnvonnxparser.so.10" ];
            installPhase = "mkdir -p $out && cp -r lib $out/";
          };

          voiceModels = pkgs.runCommand "note-voice-models" { } ''
            mkdir -p $out/nemotron $out/kokoro
            tar xjf ${pkgs.fetchurl {
              url = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25.tar.bz2";
              hash = "sha256-Cuc6Qc1RWZ3HysmsCD2dNd5T12LKRZI1Bf3kejdRgUs=";
            }} -C $out/nemotron --strip-components=1
            tar xjf ${pkgs.fetchurl {
              url = "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-en-v0_19.tar.bz2";
              hash = "sha256-kSgEhVoEdF+nejC+VFs/ml0VxNZtsAuIy81JId9gWsc=";
            }} -C $out/kokoro --strip-components=1
            rm -rf $out/nemotron/test_wavs
            cp ${pkgs.fetchurl {
              url = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx";
              hash = "sha256-niRJ4Qh0ltjUyrqQfyPgvT942R+lUkebucI6wJy7H9Y=";
            }} $out/silero_vad.onnx
            cp ${pkgs.fetchurl {
              url = "https://huggingface.co/pipecat-ai/smart-turn-v3/resolve/f766f81d3cfdf7737ac64aad813d91bbfd56bf93/smart-turn-v3.2-cpu.onnx";
              hash = "sha256-K7AmMWsUpmBIanWxczzT+6uML9AxTcmve+SfjMqWfk8=";
            }} $out/smart-turn.onnx
          '';

          # Raw 48 kHz mono s16le.
          voiceCues = pkgs.runCommand "note-voice-cues" { nativeBuildInputs = [ pkgs.ffmpeg-headless ]; } ''
            mkdir -p $out
            y=${pkgs.yaru-theme}/share/sounds/Yaru/stereo
            ffmpeg -v error -i $y/message-new-instant.oga -ac 1 -ar 48000 -f s16le $out/ready.pcm
            ffmpeg -v error -i $y/message.oga -ac 1 -ar 48000 -f s16le $out/heard.pcm
          '';

          # libwebrtc is built with clang against its own libc++, so the voice
          # crate compiles and links with LLVM.
          voiceCrane = craneLib.overrideScope (_: _: { stdenvSelector = p: p.llvmPackages_21.stdenv; });
          voiceArgs = commonArgs // {
            pname = "note-voice";
            nativeBuildInputs = [ pkgs.pkg-config pkgs.llvmPackages_21.lld pkgs.makeWrapper ];
            buildInputs = [ pkgs.openssl pkgs.glib ];
            LK_CUSTOM_WEBRTC = "${webrtc}/linux-x64-release";
            SHERPA_ONNX_LIB_DIR = "${sherpaGpu}/lib";
            cargoExtraArgs = "--locked -p note-voice";
          };
          voiceArtifacts = voiceCrane.buildDepsOnly voiceArgs;

          voice = voiceCrane.buildPackage (voiceArgs // {
            cargoArtifacts = voiceArtifacts;
            doCheck = false;
            postFixup = ''
              for b in $out/bin/*; do
                wrapProgram $b \
                  --prefix LD_LIBRARY_PATH : ${sherpaGpu}/lib:/run/opengl-driver/lib \
                  --set-default ORT_DYLIB_PATH ${sherpaGpu}/lib/libonnxruntime.so \
                  --set-default NOTE_VOICE_MODELS ${voiceModels} \
                  --set-default NOTE_VOICE_CUES ${voiceCues}
              done
            '';
            passthru = { models = voiceModels; cues = voiceCues; inherit webrtc; sherpa = sherpaGpu; };
            meta = {
              description = "Voice calls for Note over Matrix and LiveKit";
              license = lib.licenses.unlicense;
              mainProgram = "note-voice";
              platforms = [ "x86_64-linux" ];
            };
          });

          voiceTests = voiceCrane.cargoTest (voiceArgs // {
            cargoArtifacts = voiceArtifacts;
            cargoTestExtraArgs = "-p note-voice";
            SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
          });

          # The prompt tests read the shipped defaults beside the crate; HTTP
          # clients refuse to start without CA roots.
          tests = craneLib.cargoTest (commonArgs // {
            inherit cargoArtifacts;
            cargoTestExtraArgs = "--workspace --exclude note-voice";
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
        { inherit web server voice voiceTests tests desktop; };
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
          note-voice-models = b.voice.models;
          note-voice-cues = b.voice.cues;
          note-web = b.web;
          note-desktop = b.desktop;
          desktop = b.desktop;
        });

      checks = forAllSystems (pkgs:
        { note-server-tests = (build pkgs).tests; }
        // lib.optionalAttrs (pkgs.stdenv.hostPlatform.system == "x86_64-linux") {
          note-voice-tests = (build pkgs).voiceTests;
          module = pkgs.testers.runNixOSTest (import ./nix/test.nix self);
        });

      devShells = forAllSystems (pkgs:
        let v = (build pkgs).voice; in {
          default = pkgs.mkShell {
            packages = with pkgs; [ cargo rustc rustfmt clippy nodejs pnpm sqlite ];
          };
          voice = (pkgs.mkShell.override { stdenv = pkgs.llvmPackages_21.stdenv; }) {
            packages = with pkgs; [ cargo rustc rustfmt clippy ];
            nativeBuildInputs = [ pkgs.pkg-config pkgs.llvmPackages_21.lld ];
            buildInputs = [ pkgs.openssl pkgs.glib ];
            LK_CUSTOM_WEBRTC = "${v.webrtc}/linux-x64-release";
            SHERPA_ONNX_LIB_DIR = "${v.sherpa}/lib";
            LD_LIBRARY_PATH = "${v.sherpa}/lib:/run/opengl-driver/lib";
            ORT_DYLIB_PATH = "${v.sherpa}/lib/libonnxruntime.so";
            NOTE_VOICE_MODELS = "${v.models}";
            NOTE_VOICE_CUES = "${v.cues}";
          };
        });
    };
}
