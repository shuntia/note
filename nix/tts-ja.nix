# The Japanese speech sidecar: a Rust binary serving Style-Bert-VITS2 JP-Extra (小春音アミ) on
# note-voice's GPU onnxruntime, with VOICEVOX CORE (冥鳴ひまり) on the CPU behind it. The アミ
# model is exported to ONNX at build time; Python and torch stay out of the runtime closure.
# See tts-ja/README.md for provenance and licences.
{ pkgs, lib, craneLib, pyproject-nix, uv2nix, pyproject-build-systems, sherpaGpu }:

let
  hf = repo: rev: file: hash: pkgs.fetchurl {
    url = "https://huggingface.co/${repo}/resolve/${rev}/${file}";
    inherit hash;
  };
  github = path: hash: pkgs.fetchurl { url = "https://github.com/${path}"; inherit hash; };

  # The export's Python env: style-bert-vits2 on CPU torch, from tts-ja/convert/uv.lock.
  convertEnv =
    let
      python = pkgs.python311;
      workspace = uv2nix.lib.workspace.loadWorkspace { workspaceRoot = ../tts-ja/convert; };
      locked = map (p: p.name) (lib.importTOML ../tts-ja/convert/uv.lock).package;
      patchWheel = old: {
        nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [ pkgs.autoPatchelfHook ];
        buildInputs = (old.buildInputs or [ ]) ++ [ pkgs.stdenv.cc.cc.lib pkgs.zlib ];
        autoPatchelfIgnoreMissingDeps = [ "libtbb.so.12" ];
        appendRunpaths = [ "$ORIGIN" ];
      };
      overrides = final: prev:
        lib.genAttrs (lib.filter (n: prev ? ${n} && n != "note-tts-ja-convert" && n != "docopt") locked)
          (name: prev.${name}.overrideAttrs patchWheel)
        // {
          docopt = prev.docopt.overrideAttrs (old: {
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
    in
    pythonSet.mkVirtualEnv "note-tts-ja-convert-env" workspace.deps.default;

  amiRev = "7af50fba79c52110046679eb5a0c8895afc5c584";
  amiSource = pkgs.linkFarm "sbv2-koharune-ami" {
    "koharune-ami.safetensors" = hf "litagin/sbv2_koharune_ami" amiRev "koharune-ami/koharune-ami.safetensors" "sha256-mAEZ5mYPwkGxwpcBaTP0MkCz/GIDqfUR3S8trHQEKZE=";
    "config.json" = hf "litagin/sbv2_koharune_ami" amiRev "koharune-ami/config.json" "sha256-yZd2XqFbTmaIlfaa3O3UCEXdIc1+qcTUwfbaU9mgF44=";
    "style_vectors.npy" = hf "litagin/sbv2_koharune_ami" amiRev "koharune-ami/style_vectors.npy" "sha256-hnUnlS/0W2Yh/hi4tRcowpq+OuuAt04G8jXfcaqCrl0=";
  };
  ami = pkgs.runCommand "sbv2-koharune-ami-onnx" { nativeBuildInputs = [ convertEnv ]; } ''
    export HOME=$TMPDIR NUMBA_CACHE_DIR=$TMPDIR
    note-tts-ja-convert ${amiSource} $out
  '';

  # DeBERTa as ONNX, from the sbv2-api project's assets.
  assetsRev = "595cc201719ee2315251f6418ea1616135a2a5ca";
  sbv2Models = pkgs.linkFarm "note-tts-ja-sbv2" {
    "deberta.onnx" = hf "neody/sbv2-api-assets" assetsRev "deberta/deberta.onnx" "sha256-tpu7Fbd3pUTXYNAg3FXjFvZYAWd1MW69Qp9+X8zNANQ=";
    "tokenizer.json" = hf "neody/sbv2-api-assets" assetsRev "deberta/tokenizer.json" "sha256-IaF+TQAyc56C3GDxVbSuN+lFGRA+DTmWV/3SSew+eQU=";
    "model.onnx" = "${ami}/model.onnx";
    "style_vectors.json" = "${ami}/style_vectors.json";
  };

  # VOICEVOX CORE's C API, its onnxruntime build, the OpenJTalk dictionary and the voice model
  # holding 冥鳴ひまり (style 14).
  voicevox = pkgs.stdenv.mkDerivation {
    pname = "note-tts-ja-voicevox";
    version = "0.17.0";
    dontUnpack = true;
    nativeBuildInputs = [ pkgs.autoPatchelfHook pkgs.unzip ];
    buildInputs = [ pkgs.stdenv.cc.cc.lib ];
    installPhase = ''
      mkdir -p $out/lib $out/dict $out/share/doc
      unzip -j ${github "VOICEVOX/voicevox_core/releases/download/0.17.0/voicevox_core-linux-x64-0.17.0.zip" "sha256-AKgMVoji/eCTo+LBpMORcLHIZ2DZJjilVMipcanZcHc="} \
        voicevox_core-linux-x64-0.17.0/lib/libvoicevox_core.so -d $out/lib
      tar xzf ${github "VOICEVOX/onnxruntime-builder/releases/download/voicevox_onnxruntime-1.17.3/voicevox_onnxruntime-linux-x64-1.17.3.tgz" "sha256-crUof91I3IM6mSn26eOCbnk7VM4SAhgb6T9jgjoiL1g="} \
        --strip-components=1 -C $TMPDIR
      cp $TMPDIR/lib/libvoicevox_onnxruntime.so.1.17.3 $out/lib/
      cp $TMPDIR/TERMS.txt $out/share/doc/voicevox_onnxruntime-TERMS.txt
      tar xzf ${github "r9y9/open_jtalk/releases/download/v1.11.1/open_jtalk_dic_utf_8-1.11.tar.gz" "sha256-/mug5DVCzvmDOavf/ZA+BiAI6hcLBOfio12oBZAvOCo="} \
        --strip-components=1 -C $out/dict
      ln -s ${github "VOICEVOX/voicevox_vvm/releases/download/0.16.4/1.vvm" "sha256-jfIIFb6ahKS0cjued40u9qTfJ35Icv6e/ilnamDgJ3Q="} $out/model.vvm
    '';
  };

  # Build scripts of three dependencies download data; each is handed it here instead.
  kanalizerModel = hf "VOICEVOX/kanalizer-model" "223ea6044503992673ea8d24cb9c39082dab7497" "model/c2k.safetensors" "sha256-sKhunAsN9Uwz2O1+eFQN8fh09eq67cFotTtLHsWJBRM=";
  naistJdic = pkgs.runCommand "jpreprocess-naist-jdic-0.12.0" { } ''
    mkdir -p $out/0.12.0
    tar xzf ${github "jpreprocess/jpreprocess/releases/download/v0.12.0/naist-jdic-jpreprocess.tar.gz" "sha256-8mbVaXJ8761DXBobYAW2AAQLf453vCxbbUx66rx3/2A="} -C $out/0.12.0
  '';
  src = lib.fileset.toSource {
    root = ../tts-ja;
    fileset = lib.fileset.unions [ ../tts-ja/Cargo.toml ../tts-ja/Cargo.lock ../tts-ja/src ];
  };
  args = {
    inherit src;
    strictDeps = true;
    # The downloaders in those build scripts link OpenSSL, though they never run.
    nativeBuildInputs = [ pkgs.pkg-config ];
    buildInputs = [ pkgs.openssl ];
    cargoVendorDir = craneLib.vendorCargoDeps {
      inherit src;
      overrideVendorGitCheckout = ps: drv:
        if lib.any (p: p.name == "kanalizer") ps then
          drv.overrideAttrs (old: {
            nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [ pkgs.brotli ];
            postInstall = (old.postInstall or "") + ''
              mkdir -p $out/kanalizer-0.0.0/models
              cp ${kanalizerModel} $out/kanalizer-0.0.0/models/model-c2k.safetensors
              brotli -q 11 -w 22 -c ${kanalizerModel} > $out/kanalizer-0.0.0/models/model-c2k.safetensors.br
            '';
          })
        else drv;
    };
    LINDERA_CACHE = naistJdic;
    # With load-dynamic, ort only needs this set to skip its download.
    ORT_LIB_LOCATION = "${sherpaGpu}/lib";
    # sbv2_core's build script fetches a dictionary into ~/.cache/sbv2 unless one is there; the
    # feature that would embed it is off.
    preBuild = ''
      export HOME=$TMPDIR/home
      mkdir -p $HOME/.cache/sbv2
      touch $HOME/.cache/sbv2/all.bin
    '';
  };
  version = (lib.importTOML ../tts-ja/Cargo.toml).package.version;
in
craneLib.buildPackage (args // {
  pname = "note-tts-ja";
  inherit version;
  cargoArtifacts = craneLib.buildDepsOnly (args // { pname = "note-tts-ja-deps"; inherit version; });
  nativeBuildInputs = args.nativeBuildInputs ++ [ pkgs.makeWrapper ];
  postFixup = ''
    wrapProgram $out/bin/note-tts-ja \
      --prefix LD_LIBRARY_PATH : ${sherpaGpu}/lib:/run/opengl-driver/lib \
      --set-default ORT_DYLIB_PATH ${sherpaGpu}/lib/libonnxruntime.so \
      --set-default NOTE_TTS_JA_SBV2_DIR ${sbv2Models} \
      --set-default NOTE_TTS_JA_VOICEVOX_DIR ${voicevox}
  '';
  passthru = { inherit ami sbv2Models voicevox convertEnv; };
  meta = {
    description = "Japanese speech sidecar for Note's voice calls";
    license = lib.licenses.mit;
    mainProgram = "note-tts-ja";
    platforms = [ "x86_64-linux" ];
  };
})
