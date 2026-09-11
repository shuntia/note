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

With Nix, the flake builds the server and the web client together:

```sh
nix build            # ./result/bin/note-server, web client under share/note/web
```

The packaged binary defaults `web_dir` to its own bundled client, so a
`server.toml` that leaves `web_dir` unset serves the PWA from the store path.

Without Nix:

```sh
cargo build --release
```

The binary is written to `target/release/note-server` and looks for the web
client under `web/dist` (see "Web client" below).

## First run

1. Copy or edit `config/server.toml` and `config/defaults/` to taste (the
   checked-in versions work as-is for a local instance).
2. Create the first admin user:

   ```sh
   cargo run --release -- create-user <name> <password> --admin
   ```

   Further accounts come from the same command, or from the admin panel once
   its secret is installed (see "Admin panel").

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
  server.toml                       # bind_addr, public_base_url, data_dir, web_dir
  defaults/
    user.toml                       # display_name, timezone, template, nightly_time,
                                    # show_arc_between_sessions, counter
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

### Settings API

`config/users/<username>/user.toml` is also what the settings routes read and
write, so a hand-edited file and a client-side change are the same thing.

- `GET /api/settings` → the effective values (defaults merged with the user's
  file) plus the two choice lists the client renders:

  ```json
  {
    "display_name": "Aki", "timezone": "Asia/Tokyo",
    "nightly_time": "03:00", "template": "default",
    "show_arc_between_sessions": true, "counter": "remaining",
    "templates": ["default", "deep-work"],
    "timezones": ["Africa/Abidjan", "…"]
  }
  ```

  `templates` is every `.toml` stem under `defaults/templates/` plus the user's
  own `templates/`; `timezones` is the bundled IANA database.

- `PUT /api/settings` takes any subset of `display_name`, `timezone`,
  `nightly_time`, `template`, `show_arc_between_sessions` and `counter`, and
  returns the merged settings without the two lists. A rejected field is a
  `400` whose `{"error": …}` names it and leaves the file untouched:
  `display_name` is trimmed, non-blank and at most 64 characters; `timezone`
  must be an IANA name; `nightly_time` must be a zero-padded 24-hour `HH:MM`;
  `template` must be one of `templates`; `show_arc_between_sessions` is a bool;
  `counter` must be `remaining` or `elapsed`. The write replaces the user file
  with all six keys through a temp file and a rename, so a crash mid-write
  cannot leave a half-written config.

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

### Memory API

Read-only, scoped to the session's own user:

- `GET /api/memory?category=&q=&limit=` → `{"items":[{"id","category","summary"}]}`,
  live facts only. Without `q` this browses newest-first, optionally filtered to
  one of `semantic`, `episodic`, `procedural` (anything else is a 400). A
  non-blank `q` runs a lexical search instead and ignores `category`. `limit`
  defaults to 100 and is clamped to 1–200.
- `GET /api/memory/{id}` → the whole fact:
  `{"id","category","summary","body","supersedes","created","archived"}`. An
  unknown or malformed id, or one belonging to another user, is a 404.

Two more routes settle a single event from the client:

- `POST /api/events/{id}/alert {alert}` — whether that one event pings when it
  fires; a block refuses with a `409`.
- `POST /api/events/{id}/move_tomorrow` — drops the event from today and plans
  it into tomorrow, returning `{"event_id", "date"}`; an already decided event
  is a `409`.

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
OpenAI-compatible endpoint for chat — NVIDIA NIM
(`https://integrate.api.nvidia.com/v1`) included — and OpenAI-compatible for
embeddings (a local llama.cpp router works). The key comes from the file named
in `api_key_file` (bare key, surrounding whitespace ignored) or the env var
named in `api_key_env`; the file wins if both are set. The key itself never
lives in config files.

With an embeddings provider configured, memory search becomes hybrid
(lexical + vector) and degrades back to lexical automatically when the
provider is down.

`POST /api/talk {message, conversation_id?}` runs one turn with the agent.
Turns belong to persisted conversations: omit `conversation_id` and the server
opens one (titled from the message), pass one and the last 32 text turns are
replayed to the model first. The response carries the `conversation_id`, the
reply, and `steps` — every tool call the agent made, with its arguments,
result, and error flag — which the web client renders as expandable blocks.
Conversations are managed over `GET /api/conversations`,
`PATCH/DELETE /api/conversations/{id}`, and
`GET /api/conversations/{id}/messages` (user, assistant, and tool rows in
order). Agent behavior lives in editable prompt files
(`config/defaults/prompts/`, overridable per user under
`config/users/<user>/prompts/`) — changing tone or policy is a file edit, not
a deploy. The two editable prompts, `persona` and `planning`, are also served
over `GET /api/prompts/{name}` (`{name, content, custom}`, `content` being the
effective text), `PUT /api/prompts/{name} {content}` (writes the user's
override), and `DELETE /api/prompts/{name}` (drops it, back to the default);
any other name is a 404.

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

## Web client

The installable PWA lives in `web/` — React + Vite, TypeScript, with React,
React DOM, marked, and DOMPurify its only runtime dependencies (all bundled;
the client makes no external requests). Build it once and the server serves it:

```sh
cd web && pnpm install && pnpm build
```

The server looks for the build at `web_dir` from `config/server.toml`
(default `web/dist`, resolved against the working directory like `data_dir`)
and serves it with an SPA fallback; without a build, the API still runs.
Unknown `/api/*` paths stay `404` rather than falling back to the app shell.

Sign in with a user from `create-user`. The home screen is one face on the
gradient with the day a gesture away: swipe up, scroll, or press ArrowDown to
raise Today under it, and the other way to send it back. In a focus session
that face is a gauge around the time left on the step, with pause and "Done
with this step" under it; between sessions it is the next routine, named and
counted down, over an arc that fills as the wait runs out. A phone opens there;
a desktop keeps the home screen for sessions and opens on Today.

Today is the same plan with room to read it: the next event as the hero with
its actions (Start, Later, drop it, move it to tomorrow, silence it), the day
drawn as a line from 06 to 24 with every event still ahead on it, and the
morning debrief folded at the foot. The shell around all of it is a top bar on
desktop — the five views and a quick-capture field `N` focuses — and a fixed
tab bar on phones; a running session takes the whole screen and both step aside
until Today is raised. Tasks is the list with quick-add, steps, durations, and
a start that opens a session. Chat is a full conversation surface: a sidebar
of persisted conversations (new, rename, delete), assistant replies rendered
as sanitized markdown with copyable code blocks, and every tool call the agent
makes shown as an expandable block with its arguments and result. Memory
browses everything the agent has saved — filter by category or search, and
open any fact to read it. Settings gathers
the home screen (`show_arc_between_sessions`, whether the wait draws its arc,
and `counter`, whether a session reads remaining or elapsed), the day
(template, which routines ping, nightly debrief time, timezone), your name, a
System / Light / Dark theme choice, the persona editor (the assistant's system
prompt, per-user override with reset-to-default), a Web Push toggle (needs the
`[channels.webpush]` config), and the server log for admin users. Delivered
events arrive live over the WebSocket while the app is open, and as push
notifications when it is not; `web/public/sw.js` renders those notifications
and focuses an open tab when one is clicked.

For development, `pnpm dev` proxies `/api` (WebSocket included) to
`127.0.0.1:3271`.

Screens can also be captured. Build the binary once (`cargo build`), leave
`NOTE_API=http://127.0.0.1:3299 pnpm dev` running, and
`pnpm shot <today|tasks|chat|memory|settings> <WxH> <out.png>` starts a throwaway
server on that port, signs a scratch user in, and writes the PNG; `--session`
shoots with a focus session running and `--stage N` raises the home screen N
steps first.

## Admin panel

Settings shows admin-role users an "Admin panel" row. The panel manages
accounts (create, password reset, admin role, disable, sign out everywhere),
shows server status, and browses the server log. Everything it does is
written to `event_log` as `admin_*` rows naming the acting admin.

### Lockdown

Every `/api/admin/*` route needs, in order:

1. an admin-role session (`403` otherwise);
2. an *admin grant*: the panel asks for the password again plus a 6-digit TOTP
   code, and the server answers with a second cookie
   (`admin=…; Path=/api/admin; HttpOnly; SameSite=Strict`, `Secure` on HTTPS)
   valid 15 minutes, bound to that session, and deleted with it. Every route
   except `gate`, `elevate` and `drop` answers `401` without a live grant;
3. same-origin provenance: a request whose `Sec-Fetch-Site` says a foreign site
   started it is refused (`403`).

Elevation attempts are limited like logins (10 per username per 15 minutes),
TOTP codes are single-use, and admin responses are `Cache-Control: no-store`.

### Secrets

`secrets_dir` in `server.toml` (default `persist/secrets`, gitignored) holds
`admin_totp`: the base32 TOTP seed (SHA-1, 6 digits, 30 s). Generate and
enrol one with:

```sh
note-server totp-generate            # prints a seed and its otpauth:// URI
note-server totp-uri                 # the URI for the seed already installed
```

Until the file exists, a release server keeps the panel locked: the gate
reports `totp: "missing"` and elevation answers `503`. Startup writes an
`admin_locked` row so the state is visible in the log.

### Dev builds

```sh
cargo run --features dev-inspect
```

`dev-inspect` is a Cargo feature, off by default and off in the Nix package.
It compiles in the inspection routes (`/api/admin/inspect/...`: a user's
config, tasks, today's events, conversations and memory files, all editable,
plus a SQL console on the live database), lets elevation pass on the password
alone when no seed is installed, and makes the panel show a red banner. The
web client is a single build; it renders the inspection section only when the
server's gate reports `inspect: true`.

### Routes

| Route | Needs | Effect |
|---|---|---|
| `GET /api/admin/gate` | role | `{elevated, expires_at?, totp, inspect}` |
| `POST /api/admin/elevate` `{password, code?}` | role | sets the grant cookie |
| `POST /api/admin/drop` | role | ends the grant |
| `GET /api/admin/status` | grant | version, build, uptime, DB size, counts, providers |
| `GET /api/admin/users` | grant | `[{id, username, role, disabled, sessions}]` |
| `POST /api/admin/users` `{username, password, admin}` | grant | `201 {id}`; `409` taken; `422` invalid |
| `PATCH /api/admin/users/{id}` `{role?, disabled?, password?}` | grant | `409` if it would leave no enabled admin or change the actor's own role |
| `POST /api/admin/users/{id}/revoke_sessions` | grant | `{revoked}` |
| `GET /api/admin/log?limit&kind&before_id` | grant | `{rows, kinds}` |

A password reset ends the account's other sessions; disabling an account ends
all of them and blocks sign-in.

## Running as a systemd service

From the flake, install the package into your profile and run it as a user
service (no root needed; enable lingering so it survives logout):

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
WorkingDirectory=/home/shuntia/Projects/note
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

`WorkingDirectory` is where `config/` and `data/` live. The same unit works
system-wide with a host-built binary:

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
