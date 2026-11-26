# Everything Purrnel needs at run time, kept as small as we can make it.
# The same `purrnel` package goes into the container image now and the NixOS USB image later.
{ pkgs, src, releaseVersion ? "dev" }:
let
  inherit (pkgs) lib;

  # Only what cargo and `include_str!("ui/index.html")` need, so editing docs or scripts never rebuilds.
  rustSrc = lib.fileset.toSource {
    root = src;
    fileset = lib.fileset.unions [
      (src + "/Cargo.toml")
      (src + "/Cargo.lock")
      (src + "/crates")
      (src + "/ui")
    ];
  };

  # Likewise for the runtime files, so editing a script or the README does not rebuild the image.
  runtimeSrc = lib.fileset.toSource {
    root = src;
    fileset = lib.fileset.unions [
      (src + "/assets/sfx")
      (src + "/tools/tts_worker.py")
    ];
  };

  harness = pkgs.rustPlatform.buildRustPackage {
    pname = "purrnel-harness";
    version = "0.1.0";
    src = rustSrc;
    # Shown at the top of the page: the release tag, or "dev".
    PURRNEL_VERSION = releaseVersion;
    cargoLock.lockFile = src + "/Cargo.lock";
    cargoBuildFlags = [ "-p" "purrnel-harness" ];
    cargoTestFlags = [ "--workspace" ];
    nativeBuildInputs = [ pkgs.cmake ];
    # nixpkgs asks aws-lc-sys to use a system copy, which we do not have: build the bundled one.
    preBuild = "unset AWS_LC_SYS_USE_SYSTEM";
  };

  # Python for the speech worker, with onnxruntime's OpenVINO plugin (170 MB, Intel-only) taken out.
  python = pkgs.python3.override {
    packageOverrides = self: super: {
      onnxruntime = super.onnxruntime.overridePythonAttrs (old: {
        nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [ pkgs.removeReferencesTo ];
        postFixup = (old.postFixup or "") + ''
          rm -f $out/${self.python.sitePackages}/onnxruntime/capi/libonnxruntime_providers_openvino.so
          # The wheel also remembers where the C++ onnxruntime lives, only to find that plugin.
          find $out -type f -exec remove-references-to -t ${lib.getLib pkgs.openvino} -t ${pkgs.onnxruntime} {} +
        '';
      });
    };
  };

  # Piper without its training stack (torch, lightning, librosa...): inference only.
  piper = (pkgs.piper-tts.override {
    python3Packages = python.pkgs;
    # Only its voice data and library are used, not the command-line tool that needs ALSA plugins (and ffmpeg).
    espeak-ng = pkgs.espeak-ng.override { alsa-plugins = pkgs.emptyDirectory; };
    withAlignment = false;
    withHTTP = false;
    withJapanese = false;
    withTrain = false;
  }).overridePythonAttrs (_: {
    pythonImportsCheck = [ "piper" ];
    doCheck = false;
  });
  pythonEnv = python.withPackages (ps: [ (ps.toPythonModule piper) ]);

  # ffmpeg with only what we use: the rubberband pitch filter for the cat voice, and the v4l2 webcam.
  ffmpeg = (pkgs.ffmpeg_9.override {
    ffmpegVariant = "headless";
    withHeadlessDeps = false;
    withSmallDeps = false;
    withFullDeps = false;
    # Switching the dependencies off also switches off the ffmpeg program and its libraries: ask for those again.
    buildFfmpeg = true;
    buildAvcodec = true;
    buildAvdevice = true;
    buildAvfilter = true;
    buildAvformat = true;
    buildAvutil = true;
    buildSwresample = true;
    buildSwscale = true;
    withRubberband = true;
    withV4l2 = true;
  }).overrideAttrs (_: {
    # ffmpeg's own test programs need features this cut-down build leaves out.
    doCheck = false;
  });

  # The speech server only: no microphone demo programs (SDL pulls in a desktop toolkit) and no ffmpeg.
  whisper = pkgs.whisper-cpp.override {
    withSDL = false;
    withFFmpegSupport = false;
  };

  whisperModel = pkgs.fetchurl {
    url = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en-q5_1.bin";
    hash = "sha256-S69w3Q18Qke6K4H6/ZwBAFrHfC+e8GTgDc8ZXQ4v3S8=";
  };
  voiceBase = "https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/lessac/medium/en_US-lessac-medium.onnx";
  voice = pkgs.fetchurl {
    url = voiceBase;
    hash = "sha256-Xv4J5pkCGHgnr2RuGm6dJp3udp+Yd9F7FrG0buqvAZ8=";
  };
  voiceConfig = pkgs.fetchurl {
    url = voiceBase + ".json";
    hash = "sha256-7+GcQXvtBV8taZCCSMa6ZQ+hNbyGiw5quz2hgdq2kKA=";
  };

  purrnel = pkgs.runCommand "purrnel"
    {
      nativeBuildInputs = [ pkgs.makeWrapper ];
      meta.mainProgram = "purrnel";
    }
    ''
      root=$out/share/purrnel
      mkdir -p $root/assets $root/tools $root/models $out/bin
      cp -r ${runtimeSrc}/assets/sfx $root/assets/
      cp ${runtimeSrc}/tools/tts_worker.py $root/tools/
      ln -s ${whisperModel} $root/models/ggml-base.en-q5_1.bin
      ln -s ${voice} $root/models/en_US-lessac-medium.onnx
      ln -s ${voiceConfig} $root/models/en_US-lessac-medium.onnx.json

      makeWrapper ${harness}/bin/purrnel-harness $out/bin/purrnel \
        --set-default PURRNEL_ROOT $root \
        --set-default PURRNEL_WHISPER_BIN ${whisper}/bin/whisper-server \
        --set-default PURRNEL_PIPER_PYTHON ${pythonEnv}/bin/python \
        --set-default SSL_CERT_FILE ${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt \
        --prefix PATH : ${lib.makeBinPath [ ffmpeg pkgs.pulseaudio ]}
    '';

  # What the container runs first: asks for the API key and place if it has a terminal and they are
  # not set, then starts purrnel. The text is scripts/container-start.sh.
  start = pkgs.writeShellApplication {
    name = "purrnel-start";
    runtimeInputs = [ pkgs.coreutils purrnel ];
    text = builtins.readFile ../scripts/container-start.sh;
  };

  # `docker load < result`, then see scripts/docker.sh.
  docker = pkgs.dockerTools.buildLayeredImage {
    name = "purrnel";
    tag = "latest";
    contents = [ purrnel start ];
    extraCommands = ''
      mkdir -m 1777 tmp data
    '';
    config = {
      Entrypoint = [ "${start}/bin/purrnel-start" ];
      Env = [
        "PURRNEL_BIND=0.0.0.0"
        "PURRNEL_CONFIG=/data/config"
        "PURRNEL_DATA=/data/artefacts"
        "HOME=/data"
      ];
      ExposedPorts."7979/tcp" = { };
      Volumes."/data" = { };
    };
  };
in
{
  inherit harness purrnel docker ffmpeg piper pythonEnv whisper;
}
