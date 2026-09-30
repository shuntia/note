# services.note — the server as a system service.
#
# State (the SQLite database, memory files, per-user settings and prompt
# overrides) lives under `stateDir`; the shipped defaults and web client come
# from the package. Secrets never enter the Nix store: each entry of
# `credentials` is handed over with systemd LoadCredential, so the source files
# can stay root-only (e.g. under /run/secrets/note) and the service reads
# them from /run/credentials/note.service/<name>.
self:
{ config, lib, pkgs, ... }:

let
  cfg = config.services.note;
  toml = pkgs.formats.toml { };
  credDir = "/run/credentials/note.service";
  voiceCfg = cfg.voice;
  voiceSocket = "/run/note/voice.sock";
  voiceToml = toml.generate "note-voice.toml" (voiceCfg.settings // {
    socket = voiceSocket;
    state_dir = "/var/lib/note-voice";
    token_file = "/run/credentials/note-voice.service/matrix-bot.token";
  });
  serverToml = toml.generate "server.toml"
    (lib.recursiveUpdate cfg.settings (lib.optionalAttrs voiceCfg.enable { voice.socket = voiceSocket; }));
  env = {
    NOTE_SERVER_CONFIG = "${serverToml}";
    NOTE_CONFIG_DIR = "${cfg.stateDir}/config";
    NOTE_DEFAULTS_DIR = "${cfg.package}/share/note/defaults";
  };
  # Runs a CLI subcommand (create-user, set-category, totp-*) against the
  # service's own config and state, as the service user.
  ctl = pkgs.writeShellScriptBin "note-ctl" ''
    exec ${pkgs.util-linux}/bin/runuser -u ${cfg.user} -- \
      ${pkgs.coreutils}/bin/env --chdir=${cfg.stateDir} \
      ${lib.concatStringsSep " " (lib.mapAttrsToList (k: v: "${k}=${lib.escapeShellArg v}") env)} \
      ${lib.getExe cfg.package} "$@"
  '';
in
{
  options.services.note = {
    enable = lib.mkEnableOption "the Note daily planning server";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.note-server;
      defaultText = lib.literalExpression "note.packages.\${system}.note-server";
    };

    user = lib.mkOption { type = lib.types.str; default = "note"; };
    group = lib.mkOption { type = lib.types.str; default = "note"; };

    stateDir = lib.mkOption {
      type = lib.types.path;
      default = "/var/lib/note";
      description = "Database, memory files and per-user config. Persist this directory.";
    };

    credentials = lib.mkOption {
      type = lib.types.attrsOf lib.types.path;
      default = { };
      example = {
        "openrouter.key" = "/run/secrets/note/openrouter.key";
        "vapid.pem" = "/run/secrets/note/vapid.pem";
        admin_totp = "/run/secrets/note/admin_totp";
      };
      description = ''
        Secret files loaded with LoadCredential and readable by the service at
        ${credDir}/<name>. `settings.secrets_dir` points there, so an
        `admin_totp` entry installs the legacy admin seed; reference the others
        from settings via `config.services.note.credentialPath "<name>"`.
      '';
    };

    credentialPath = lib.mkOption {
      type = lib.types.functionTo lib.types.str;
      readOnly = true;
      default = name: "${credDir}/${name}";
    };

    settings = lib.mkOption {
      type = lib.types.submodule {
        freeformType = toml.type;
        options = {
          bind_addr = lib.mkOption { type = lib.types.str; default = "127.0.0.1:3271"; };
          public_base_url = lib.mkOption {
            type = lib.types.str;
            example = "https://note.example.com";
          };
          data_dir = lib.mkOption { type = lib.types.str; default = "${cfg.stateDir}/data"; };
          secrets_dir = lib.mkOption { type = lib.types.str; default = credDir; };
        };
      };
      default = { };
      description = "server.toml, as in config/server.toml of the repository.";
    };

    environmentFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = "Extra environment (e.g. ANTHROPIC_API_KEY for a provider's api_key_env).";
    };

    openFirewall = lib.mkOption { type = lib.types.bool; default = false; };

    voice = {
      enable = lib.mkEnableOption "the voice service that rings a linked Matrix account";
      package = lib.mkOption {
        type = lib.types.package;
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.note-voice;
        defaultText = lib.literalExpression "note.packages.\${system}.note-voice";
      };
      settings = lib.mkOption {
        type = lib.types.submodule {
          freeformType = toml.type;
          options = {
            homeserver = lib.mkOption { type = lib.types.str; example = "https://matrix.example.com"; };
            livekit_service_url = lib.mkOption { type = lib.types.str; };
          };
        };
        default = { };
        description = "note-voice.toml; socket, state_dir and token_file are filled in.";
      };
      tokenFile = lib.mkOption {
        type = lib.types.path;
        description = "The bot account's access token, handed over with LoadCredential.";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    users.users = lib.mkMerge [
      (lib.mkIf (cfg.user == "note") {
        note = { isSystemUser = true; group = cfg.group; home = cfg.stateDir; };
      })
      (lib.mkIf voiceCfg.enable {
        note-voice = { isSystemUser = true; group = cfg.group; };
      })
    ];
    users.groups = lib.mkIf (cfg.group == "note") { note = { }; };

    environment.systemPackages = [ ctl ];

    systemd.tmpfiles.settings."10-note" = {
      ${cfg.stateDir}.d = { user = cfg.user; group = cfg.group; mode = "0700"; };
      "${cfg.stateDir}/config".d = { user = cfg.user; group = cfg.group; mode = "0700"; };
    };

    systemd.services.note = {
      description = "Note daily planning server";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      environment = env // { RUST_LOG = lib.mkDefault "info"; };
      unitConfig.StartLimitIntervalSec = 0;
      serviceConfig = {
        ExecStart = lib.getExe cfg.package;
        User = cfg.user;
        Group = cfg.group;
        WorkingDirectory = cfg.stateDir;
        LoadCredential = lib.mapAttrsToList (name: path: "${name}:${path}") cfg.credentials;
        EnvironmentFile = lib.mkIf (cfg.environmentFile != null) cfg.environmentFile;
        Restart = "always";
        RestartSec = 3;
        RuntimeDirectory = "note";
        RuntimeDirectoryMode = "0750";

        ReadWritePaths = [ cfg.stateDir ];
        UMask = "0077";
        NoNewPrivileges = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectKernelLogs = true;
        ProtectControlGroups = true;
        ProtectClock = true;
        ProtectHostname = true;
        ProtectProc = "invisible";
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        SystemCallArchitectures = "native";
        SystemCallFilter = [ "@system-service" "~@privileged" ];
        CapabilityBoundingSet = "";
      };
    };

    systemd.services.note-voice = lib.mkIf voiceCfg.enable {
      description = "Note voice service";
      wantedBy = [ "multi-user.target" ];
      after = [ "note.service" "network-online.target" ];
      wants = [ "network-online.target" ];
      environment.NOTE_VOICE_CONFIG = "${voiceToml}";
      unitConfig.StartLimitIntervalSec = 0;
      serviceConfig = {
        ExecStart = lib.getExe voiceCfg.package;
        User = "note-voice";
        Group = cfg.group;
        StateDirectory = "note-voice";
        StateDirectoryMode = "0700";
        LoadCredential = [ "matrix-bot.token:${voiceCfg.tokenFile}" ];
        Restart = "always";
        RestartSec = 2;
        RestartSteps = 5;
        RestartMaxDelaySec = 60;
        UMask = "0077";
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectKernelLogs = true;
        ProtectControlGroups = true;
        ProtectClock = true;
        ProtectHostname = true;
        ProtectProc = "invisible";
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        SystemCallArchitectures = "native";
        SystemCallFilter = [ "@system-service" "~@privileged" ];
        CapabilityBoundingSet = "";
      };
    };

    networking.firewall.allowedTCPPorts = lib.mkIf cfg.openFirewall [
      (lib.toInt (lib.last (lib.splitString ":" cfg.settings.bind_addr)))
    ];
  };
}
