# Development

## Toolchain

Either a host Rust toolchain via [rustup](https://rustup.rs) (stable) plus
Node and pnpm, or the flake's shells:

- `nix develop`: `cargo`, `rustc`, `rustfmt`, `clippy`, `nodejs`, `pnpm`, `sqlite`.
- `nix develop .#voice`: the toolchain for the `note-voice` crate, with the
  LiveKit WebRTC build, sherpa-onnx, onnxruntime, the speech models
  (`NOTE_VOICE_MODELS`) and the call cues (`NOTE_VOICE_CUES`) wired in. The
  voice crate does not build outside it.

## Build

```sh
nix build                          # ./result/bin/note-server, web client under share/note/web
cargo build --release -p note-server   # target/release/note-server, serves web/dist
```

The Nix package defaults `web_dir` to its bundled client, so a `server.toml`
that leaves `web_dir` unset serves the PWA from the store path. Other flake
packages: `note-voice` (with `note-voice-models` and `note-voice-cues`),
`note-web`, `note-desktop`, and on x86_64-linux `note-tts-chatterbox` and
`note-tts-ja`.

GitHub Actions builds the desktop app for Linux, macOS and Windows
(`.github/workflows/desktop.yml`; see [web.md](web.md#desktop)). To release
it, push a `vX.Y.Z` tag; the build takes its version from the tag. Then set
`desktop/package.json`'s version past it, since nightlies are
`<that version>-nightly.<date>` and must sort above the last release.

## First run

1. Edit `config/server.toml` and `config/defaults/` to taste; the checked-in
   versions work as-is for a local instance. See [configuration.md](configuration.md).
2. Create the first admin, then start the server:

   ```sh
   cargo run --release -p note-server -- create-user <name> <password> --admin
   cargo run --release -p note-server
   ```

   It binds `bind_addr` (default `127.0.0.1:3271`) and keeps its SQLite
   database under `data_dir` (default `./data`, gitignored).

## CLI

```
note-server create-user <name> <password> [--admin] [--test]
note-server invite [--admin] [--name <username>] [--days <n>]
note-server set-category <name> <member|test>
note-server totp-generate          # a legacy admin seed and its otpauth:// URI
note-server totp-uri               # the URI for the seed already installed
```

`--test` creates an account with every background feature off (see
[account categories](configuration.md#account-categories)). On NixOS the same
commands run as `sudo note-ctl …` against the service's state.

## Tests

```sh
cd server && cargo nextest run     # server, unit and HTTP integration tests
cd web && pnpm test                # web (vitest)
nix develop .#voice -c cargo test -p note-voice
nix flake check                    # the packaged test suites and the NixOS VM test
```

`cargo-nextest` is not in the dev shell: install it (`cargo install
cargo-nextest`) or run `cargo test`.

Use the `aitest` account for test data; no other account's data may be read or
modified.

## Web development

`pnpm dev` proxies `/api` (WebSocket included) to `NOTE_API`, default
`http://127.0.0.1:3271`.

Screenshots: build the binary once (`cargo build -p note-server`), leave
`NOTE_API=http://127.0.0.1:3299 pnpm dev` running, then

```sh
pnpm shot <today|tasks|chat|memory|settings> <WxH> <out.png> [--session] [--stage N]
```

starts a throwaway server on port 3299 (`NOTE_PORT`), signs a scratch user in
and writes the PNG. `--session` shoots with a focus session running;
`--stage N` raises the home screen N steps first.

Web call check: with `target/debug/note-server` built and the same `pnpm dev`
running,

```sh
node scripts/call-check.mjs
```

starts a throwaway server with `[voice]` on a temporary socket, stands a fake
voice side on it (`scripts/fake-voice.mjs`), and drives headless Chromium
(`CHROMIUM`, default `chromium` on `PATH`) with a fake microphone: it signs in,
calls from Chat, and checks the call reaches listening, carries audio, draws
the circle and ends cleanly. It exits non-zero on the first failure.

## Inspection build

```sh
cargo run -p note-server --features dev-inspect
```

`dev-inspect` is off by default and off in the Nix package. It compiles in
`/api/admin/inspect/...` (a user's config, tasks, today's events,
conversations and memory files, all editable, plus a SQL console on the live
database), lets admin elevation pass on the password alone for an account with
no factor and no seed, and makes the admin panel show a red banner. The web
client renders the inspection section only when the gate reports
`inspect: true`.
