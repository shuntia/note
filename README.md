# Note

**This app is completely coded by an LLM and is intended to serve me specifically. Features are completely subject to change.**

A self-hosted personal planning assistant. Note keeps your tasks, calendar and
memory, plans each day overnight from a template and sets the order to work
through, wakes on its own through the day to tidy and check in over the web
app, Web Push, Matrix or a voice call, and talks it all through with an
LLM agent that acts through typed tools.

## Repository layout

| Path | What it is |
|---|---|
| `server/` | `note-server`: the Rust HTTP/WebSocket server, scheduler, agent and SQLite store |
| `voice/` | `note-voice`: the Matrix/LiveKit call service with streaming STT and TTS |
| `voice-proto/` | the framed protocol between `note-server` and `note-voice` |
| `tts-ja/` | `note-tts-ja`: the Japanese speech sidecar (Style-Bert-VITS2 + VOICEVOX) |
| `tts-sidecar/` | `note-tts-chatterbox`: the Chatterbox speech sidecar (Python, GPU) |
| `web/` | the PWA: React + Vite + TypeScript |
| `desktop/` | the Electron desktop shell around the web app |
| `config/` | `server.toml` and the shipped `defaults/` (user settings, templates, prompts, built-in memory) |
| `nix/` | the NixOS module, its VM test, and the speech-sidecar packages |
| `docs/` | reference documentation (below) |

## Quick start

```sh
nix develop                      # cargo, rustc, clippy, node, pnpm, sqlite
cargo run --release -p note-server -- create-user <name> <password> --admin
cargo run --release -p note-server  # serves 127.0.0.1:3271, data under ./data
```

Web client, with `/api` (WebSocket included) proxied to `127.0.0.1:3271`:

```sh
cd web && pnpm install && pnpm dev     # or `pnpm build` for the server to serve web/dist
```

Tests:

```sh
cd server && cargo nextest run   # server
cd web && pnpm test              # web
nix develop .#voice              # then `cargo test -p note-voice`; the voice crate needs this shell
```

With no `[providers.llm]` the server runs on a built-in mock provider, fully
offline. `nix build` produces `note-server` with the web client bundled.

## Deployment

The flake exports `nixosModules.default` (`services.note`, with optional voice
and speech sidecars). See [docs/deployment.md](docs/deployment.md).

## Documentation

- [Setup](docs/setup.md): the short version — run it, sign in, see what it does
- [Development](docs/development.md): build, dev shells, tests, screenshots, CLI
- [Configuration](docs/configuration.md): `server.toml`, `user.toml`, the settings API, account categories
- [Planning](docs/planning.md): tasks, deadlines, urgency, the run order, the daily plan, calendar and quiet windows
- [Agent](docs/agent.md): providers, sessions, the nightly run, wake-ups, context, working notes, memory, tools, prompts
- [Delivery](docs/delivery.md): the delivery ladder, WebSocket, ringing, check-in threads, Web Push, Matrix
- [Importing](docs/importing.md): mirroring outside work in, briefing tasks, reading inbox items
- [Share links](docs/share-links.md): letting someone chat with Note about a slice of your day
- [Security](docs/security.md): sign-in limits, API tokens, the admin panel, passkeys and TOTP
- [Voice](docs/voice.md): web and Matrix calls, the voice service, speech sidecars
- [Web and desktop](docs/web.md): the PWA and the Electron shell
- [API](docs/api.md): every HTTP route, with the ones not covered elsewhere
- [Deployment](docs/deployment.md): the NixOS module, other hosts, upgrades

## License

[MIT](LICENSE).
