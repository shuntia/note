# Note

A self-hosted daily planning server: tasks, a generated daily plan of events,
and channel delivery driven by per-user templates.

## Prerequisites

Either:

- A host Rust toolchain via [rustup](https://rustup.rs) (stable, 2021
  edition), or
- `nix develop` from the repo root, which drops you into a shell with
  `cargo`, `rustc`, `rustfmt`, `clippy`, and `sqlite` already on `PATH`.

## Build

```sh
cargo build --release
```

The binary is written to `target/release/note-server`.

## First run

1. Copy or edit `config/server.toml` and `config/defaults/` to taste (the
   checked-in versions work as-is for a local instance).
2. Create the first admin user:

   ```sh
   cargo run --release -- create-user <name> <password> --admin
   ```

3. Start the server:

   ```sh
   cargo run --release
   ```

   It binds to `bind_addr` from `config/server.toml` (default
   `127.0.0.1:3271`) and stores its SQLite database under `data_dir`
   (default `./data`, already gitignored).

`NOTE_CONFIG_DIR` overrides the config directory (default `./config`).

## Config layout

```
config/
  server.toml                       # bind_addr, public_base_url, data_dir
  defaults/
    user.toml                       # display_name, timezone, template
    templates/
      default.toml                  # default event template
  users/
    <username>/
      user.toml                     # per-user overrides, merged over defaults
      templates/
        <name>.toml                 # per-user template overrides
```

Per-user files are optional; anything not overridden falls back to the
`defaults/` tree.

## Memory & agent tools

Per-user long-term memory lives under `data/memory/<user>/{semantic,episodic,procedural,archive}/` —
one markdown fact per file, frontmatter with a one-line summary. Facts are
never deleted: superseding a fact writes a replacement and moves the old file
to `archive/`. A SQLite FTS index over these files is derived and rebuilt at
startup, so the files themselves are the backup-worthy source of truth.

The standing context document each agent session sees is
`config/users/<user>/standing.md`; agents edit it in place through the
`context_edit` tool, so its history is whatever your config dir's VCS says.

Model-facing capabilities are typed tool calls dispatched through a
per-session-type registry (check-in < talk < nightly). Every call is
validated, size-capped, and transactional; failures return typed rejections
to the model and never leave partial state.

Event scheduling semantics: `fixed` events cannot move; `slide` events can be
slid within ±`slide_window_min` minutes of their template time (0 = unbounded);
`drop` events can additionally be dropped by the agent. Snoozing is separate:
any undecided event can be snoozed ("not now"), which re-fires it later and is
not bounded by the slide window.

## Providers & the agent

With no LLM configured the server runs against a null provider — no API keys,
fully runnable offline; plans are still generated from templates and nights
end with the fallback debrief. Configure real providers in `config/server.toml`
(`[providers.llm]`, `[providers.embeddings]`): Anthropic or any
OpenAI-compatible endpoint for chat, OpenAI-compatible for embeddings (a
local llama.cpp router works). Keys are read from the env var named in
`api_key_env`, never from config files.

With an embeddings provider configured, memory search becomes hybrid
(lexical + vector) and degrades back to lexical automatically when the
provider is down.

`POST /api/talk {message}` runs a text conversation with the agent. Agent
behavior lives in editable prompt files (`config/defaults/prompts/`,
overridable per user under `config/users/<user>/prompts/`) — changing tone
or policy is a file edit, not a deploy.

Every night at each user's `nightly_time` (default 03:00, their timezone),
the server generates the day's plan from their template, lets the agent
adjust it and write a morning debrief, and stores the debrief. If the model
is unreachable, the plan still exists and a fallback debrief says so — a
plainer day, never a missing one.

## Channels & delivery

When an event fires, the server walks a delivery ladder: connected WebSocket
clients first, then Web Push. The first channel that accepts the message wins;
if none does, the day is plainer, never an error.

Events carry a `channel` of `push` or `voice`. Voice is not implemented in this
release — a `voice` event logs `voice_unavailable` and then takes the same
ladder.

### In-app delivery

`GET /api/ws` upgrades to a WebSocket for the session's user (cookie auth) and
receives one JSON frame per delivery:

```json
{ "type": "event", "title": "Check-in", "body": "checkin at 09:00", "urgency": "high", "event_id": 7 }
```

Delivery is one-way in v1: inbound frames are drained and ignored. The server
pings every 30s and tears down a connection that has sent nothing for 90s, so a
half-open socket stops absorbing deliveries and the ladder falls through to
Web Push.

### Web Push

Web Push needs a VAPID keypair (any P-256 EC key):

```sh
openssl ecparam -genkey -name prime256v1 -noout -out config/vapid.pem
```

Then uncomment the stanza in `config/server.toml`:

```toml
[channels.webpush]
vapid_pem_file = "config/vapid.pem"
subject = "mailto:admin@example.com"
```

`vapid_pem_file` resolves against the server's working directory, like
`data_dir`; `subject` must be a `mailto:` or `https://` contact URL, and an
unreadable key or bad subject fails startup rather than the first delivery.
Without the section, only in-app WebSocket delivery is active and every event
for a disconnected user is logged as `delivery_degraded`.

Subscription routes — all cookie-authenticated, the public-key route included:

- `GET /api/push/vapid_public_key` → `{"key": "<base64url>"}`, or `404` when
  Web Push is not configured. Pass the key to
  `pushManager.subscribe({ userVisibleOnly: true, applicationServerKey })`.
- `POST /api/push/subscribe` — the browser's `PushSubscription.toJSON()` shape:

  ```json
  { "endpoint": "https://push.example.net/send/abc", "keys": { "p256dh": "…", "auth": "…" } }
  ```

  Subscribing again with the same endpoint replaces the stored keys.
  Non-`http(s)` endpoints, or fields over 2048 (`endpoint`) / 256 (`p256dh`) /
  64 (`auth`) characters, are rejected with `400`.
- `POST /api/push/unsubscribe {endpoint}` — `404` if the endpoint is not one of
  the caller's.

Payloads are encrypted (`aes128gcm`) and VAPID-signed. Endpoints the push
service reports gone (404/410) are pruned automatically.

### Delivery in the admin log

Every outcome lands in `event_log`, readable at `GET /api/admin/log`:

- `delivery_ok` — `event <id> via ws` or `via webpush`.
- `delivery_degraded` — no channel could reach the user; the detail carries
  each channel's reason, or `no channels configured`.
- `voice_unavailable` — a `voice` event fell back to the delivery ladder
  (WebSocket first, then Web Push).

### Sessions & limits

- `POST /api/login {username, password}` sets an HttpOnly, SameSite=Lax session
  cookie valid 30 days (`Secure` whenever `public_base_url` is `https://`).
  `POST /api/logout` deletes the session row and clears the cookie.
- Login is capped at 10 attempts per username per 15-minute window; beyond that
  the route returns `429` without touching the database. A successful login
  clears the counter.
- `POST /api/talk` runs one session per user (a second concurrent request gets
  `409`) and four across the server (`503` with `Retry-After: 5` beyond that).

## Admin API

Admin-role users (see `create-user --admin`) get two extra routes:

- `GET /api/admin/log?limit=100` — the last N `event_log` rows.
- `POST /api/admin/users {username, password, admin}` — create a user.

Both return `403 Forbidden` for non-admin users.

## Running as a systemd service

```ini
[Unit]
Description=Note server
After=network.target

[Service]
ExecStart=/home/shuntia/Projects/note/target/release/note-server
WorkingDirectory=/home/shuntia/Projects/note
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

Install to `/etc/systemd/system/note.service`, then:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now note
```
