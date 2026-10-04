# The Chatterbox TTS sidecar: a Python env built from tts-sidecar/uv.lock with
# prebuilt wheels (torch from the cu124 index), plus the model weights and the
# reference voices, so the service needs no network at run time. libcuda.so.1
# comes from the host driver.
{ pkgs, lib, pyproject-nix, uv2nix, pyproject-build-systems }:

let
  python = pkgs.python311;
  workspace = uv2nix.lib.workspace.loadWorkspace { workspaceRoot = ../tts-sidecar; };

  sitePackages = "lib/python3.11/site-packages";
  locked = map (p: p.name) (lib.importTOML ../tts-sidecar/uv.lock).package;
  ffmpeg = map (l: "${l}.so.*") [ "libavutil" "libavcodec" "libavformat" "libavdevice" "libavfilter" "libswscale" "libswresample" ];
  ignoreMissing = {
    torchaudio = ffmpeg ++ [ "libsox.so" ];
    numba = [ "libtbb.so.12" ];
  };

  # Wheels link against each other's libraries (torch against the nvidia-*
  # wheels, torchaudio against torch), so each one is patched against the
  # library directories of its direct dependencies.
  patchWheel = final: name: old: {
    nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [ pkgs.autoPatchelfHook ];
    buildInputs = (old.buildInputs or [ ]) ++ [ pkgs.stdenv.cc.cc.lib pkgs.zlib ];
    autoPatchelfIgnoreMissingDeps = [ "libcuda.so.1" ] ++ ignoreMissing.${name} or [ ];
    # cuDNN and others dlopen sibling libraries by name.
    appendRunpaths = [ "$ORIGIN" ];
    preFixup = (old.preFixup or "") + lib.concatMapStrings (dep: ''
      while IFS= read -r d; do addAutoPatchelfSearchPath "$d"; done \
        < <(find ${final.${dep}}/${sitePackages} -name '*.so*' -printf '%h\n' | sort -u)
    '') (lib.attrNames (old.passthru.dependencies or { }));
  };

  overrides = final: prev:
    lib.genAttrs (lib.filter (n: prev ? ${n} && n != "note-tts-chatterbox" && n != "antlr4-python3-runtime") locked)
      (name: prev.${name}.overrideAttrs (patchWheel final name))
    // {
      soundfile = prev.soundfile.overrideAttrs (old: {
        postInstall = (old.postInstall or "") + ''
          substituteInPlace $out/${sitePackages}/soundfile.py \
            --replace-fail "_find_library('sndfile')" "'${lib.getLib pkgs.libsndfile}/lib/libsndfile.so'"
        '';
      });
      antlr4-python3-runtime = prev.antlr4-python3-runtime.overrideAttrs (old: {
        nativeBuildInputs = old.nativeBuildInputs ++ final.resolveBuildSystem { setuptools = [ ]; };
      });
    };

  pythonSet = (pkgs.callPackage pyproject-nix.build.packages { inherit python; }).overrideScope (
    lib.composeManyExtensions [
      pyproject-build-systems.overlays.default
      (workspace.mkPyprojectOverlay { sourcePreference = "wheel"; })
      overrides
    ]
  );

  env = pythonSet.mkVirtualEnv "note-tts-chatterbox-env" workspace.deps.default;

  hf = file: hash: pkgs.fetchurl {
    url = "https://huggingface.co/ResembleAI/chatterbox/resolve/5bb1f6ee58e50c3b8d408bc82a6d3740c2db6e18/${file}";
    inherit hash;
  };
  model = pkgs.linkFarm "chatterbox-model" {
    "ve.safetensors" = hf "ve.safetensors" "sha256-8JIcq0Uvoni8Jc0j/9WdNvgW19xRgd0b75dRp/th9jw=";
    "t3_cfg.safetensors" = hf "t3_cfg.safetensors" "sha256-kUyxaW9HUn/ohSyo8f4fpjyzT3b5xxXoTgZ7dE3Q2oE=";
    "s3gen.safetensors" = hf "s3gen.safetensors" "sha256-K3gQPGVCBzk5VeSQCqwUoS3o7yX0sJQk8e+RlB8WHU4=";
    "tokenizer.json" = hf "tokenizer.json" "sha256-1x46ROq7F4Tfmmjp+VslHsvxp69qn1CDWFayyp2MFKU=";
    "conds.pt" = hf "conds.pt" "sha256-ZVLXBWiDNii6AZxrA0Wed/5xyhl9XFYM75QRvunYf04=";
  };

  # See tts-sidecar/voices/README.md for provenance and licences.
  voiceZero = file: hash: pkgs.fetchurl {
    url = "https://raw.githubusercontent.com/OwenTyme/voice-zero/490cfbee850a6d409076f477c766f567000a79b6/voices/${file}";
    inherit hash;
  };
  voices = pkgs.linkFarm "chatterbox-voices" {
    "voices.json" = ../tts-sidecar/voices/voices.json;
    "carrie.flac" = voiceZero "carrie_mae_streb.flac" "sha256-NwqxwuqHSCoeiAl9vHP/0+HL9vf90krC5Ux47kLc3wU=";
    "caro.flac" = voiceZero "caro_davy.flac" "sha256-SXnQ9PXug+cADI86AELotecAv4aMgK/r5bB4WTjOWe4=";
    "bill.flac" = voiceZero "bill_boerst.flac" "sha256-KIfBhKwNuCfugOxnMalU1PAutcG+fBgeHV8w1uO943s=";
    "stuart.flac" = voiceZero "stuart_bell.flac" "sha256-5zIOKd+Wv7tD9JM5P9ht9HlQezouo04gfc/J/o6kwQY=";
  };
in
pkgs.runCommand "note-tts-chatterbox-${(lib.importTOML ../tts-sidecar/pyproject.toml).project.version}"
  {
    nativeBuildInputs = [ pkgs.makeWrapper ];
    passthru = { inherit env model voices; };
    meta = {
      description = "Chatterbox TTS sidecar for Note's voice calls";
      license = lib.licenses.unlicense;
      mainProgram = "note-tts-chatterbox";
      platforms = [ "x86_64-linux" ];
    };
  } ''
  makeWrapper ${env}/bin/note-tts-chatterbox $out/bin/note-tts-chatterbox \
    --prefix LD_LIBRARY_PATH : /run/opengl-driver/lib \
    --set HF_HUB_OFFLINE 1 \
    --set-default NOTE_TTS_MODEL_DIR ${model} \
    --set-default NOTE_TTS_VOICES_DIR ${voices}
''
