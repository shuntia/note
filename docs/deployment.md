# Deployment

## NixOS

The flake exports `nixosModules.note` (also `nixosModules.default`), which
runs the server as its own `note` user with every secret handed over through systemd `LoadCredential`, so
none reaches the Nix store.

```nix
# flake.nix
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.note.url = "github:<owner>/note";
  inputs.note.inputs.nixpkgs.follows = "nixpkgs";

  outputs = { nixpkgs, note, ... }: {
    nixosConfigurations.<host> = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";          # or aarch64-linux
      modules = [ note.nixosModules.default ./configuration.nix ];
    };
  };
}
```

```nix
# configuration.nix
services.note = {
  enable = true;
  credentials = {
    "openrouter.key" = "/run/secrets/note/openrouter.key";
    "vapid.pem"      = "/run/secrets/note/vapid.pem";
    admin_totp       = "/run/secrets/note/admin_totp";   # optional legacy seed
  };
  settings = {
    public_base_url = "https://note.example.com";
    providers.llm = {
      kind = "openai";
      base_url = "https://openrouter.ai/api/v1";
      model = "deepseek/deepseek-v4-flash";
      api_key_file = "/run/credentials/note.service/openrouter.key";
    };
    channels.webpush = {
      vapid_pem_file = "/run/credentials/note.service/vapid.pem";
      subject = "mailto:admin@example.com";
    };
  };
};
```

Minimal, for a server reached on the host itself:

```nix
services.note.enable = true;    # http://127.0.0.1:3271, mock provider
```

`settings.public_base_url` defaults to `http://<bind_addr>`; set it to the
address people use. Without `providers` the server runs on the built-in mock.

| Option | |
|---|---|
| `settings` | `server.toml` as Nix; `bind_addr` defaults to `127.0.0.1:3271`, `public_base_url` to `http://<bind_addr>`, `data_dir` to `/var/lib/note/data`, `secrets_dir` to the credentials directory |
| `credentials` | name → path; readable by the service at `/run/credentials/note.service/<name>` (`config.services.note.credentialPath "<name>"`). An `admin_totp` entry installs the legacy admin seed |
| `environmentFile` | extra environment, e.g. `ANTHROPIC_API_KEY` for a provider's `api_key_env` |
| `openFirewall` | opens the `bind_addr` port |
| `stateDir` | `/var/lib/note`: database, memory files, per-user settings and "About you" text. The one directory to back up or, on an impermanent root, persist |
| `package`, `user`, `group` | default: this flake's `note-server`, `note`, `note` |

`overlays.default` adds `note-server` and `note-voice` to `pkgs`.

`note-ctl` runs the CLI against the service's state as its user:

```sh
sudo note-ctl create-user <name> <password> --admin
```

### Voice and speech

```nix
services.note.voice = {
  enable = true;
  tokenFile = "/run/secrets/note/matrix-bot.token";   # the bot's access token
  settings = {
    homeserver = "https://matrix.example.com";
    livekit_service_url = "https://…";
  };
};
services.note.tts.chatterbox.enable = true;   # NVIDIA GPU; port 8890
services.note.tts.japanese.enable = true;     # port 8891
```

`voice.settings` is `note-voice.toml`; `socket`, `state_dir`
(`/var/lib/note-voice`), `token_file`, `models_dir`, `cues_dir` and the
sidecar list are filled in, and the server gets `[voice] socket =
"/run/note/voice.sock"`. Each sidecar takes `port`, `package` and `environment`
(Chatterbox: `NOTE_TTS_EXAGGERATION`, `NOTE_TTS_CFG_WEIGHT`,
`NOTE_TTS_TEMPERATURE`, `NOTE_TTS_VOICES_DIR`; Japanese: `NOTE_TTS_DEVICE=cpu`,
`NOTE_TTS_JA_SBV2=off`); the Japanese one also takes `sidecarId` (default
`ja`). See [voice.md](voice.md).

### Desktop app

`nixosModules.note-desktop` (and `homeManagerModules.note-desktop`) install the
desktop app:

| Option | |
|---|---|
| `programs.note-desktop.enable` | install `note-desktop` |
| `programs.note-desktop.url` | the server it opens until the user picks another; null (the default) asks on first run. On NixOS, when `services.note` is enabled on the same host, it defaults to `services.note.settings.public_base_url` |
| `programs.note-desktop.package` | default: this flake's `note-desktop` |

A desktop that also runs the server needs only:

```nix
imports = [ note.nixosModules.note note.nixosModules.note-desktop ];
services.note = { enable = true; settings.public_base_url = "https://note.example.com"; };
programs.note-desktop.enable = true;
```

A desktop that uses a server elsewhere:

```nix
imports = [ note.nixosModules.note-desktop ];
programs.note-desktop = { enable = true; url = "https://note.example.com"; };
```

### Upgrades

Back up `/var/lib/note/data/note.db`, then:

```sh
nix flake update note
sudo nixos-rebuild switch --flake .#<host>
```

Database migrations run when the service restarts.

## Behind Cloudflare

Share-link visit locations and per-address limits read Cloudflare headers
(`cf-connecting-ip`, `cf-ipcity`, `cf-ipcountry`, `cf-iplatitude`,
`cf-iplongitude`); enable the zone's *Add visitor location headers* managed
transform. A tunnel allows a response 100 seconds, which the provider
`timeout_secs` default (45) stays under.

## Other hosts

Install the package into a profile and run it as a user service (enable
lingering so it survives logout):

```sh
nix profile add .#note-server        # later: nix profile upgrade note-server
```

```ini
# ~/.config/systemd/user/note.service
[Unit]
Description=Note server
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=0

[Service]
WorkingDirectory=%h/note
ExecStart=%h/.nix-profile/bin/note-server
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now note
```

`WorkingDirectory` holds `config/` and `data/`; relative paths in `server.toml`
resolve against it.
