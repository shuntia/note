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
   its secret is installed (see "Admin panel"). An account that only exercises
   the API is created with `--test`, which starts it with every background
   feature off (see "Categories & background features"):

   ```sh
   cargo run --release -- create-user aitest <password> --test
   note-server set-category aitest test   # or move an existing account
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
  server.toml                       # bind_addr, public_base_url, data_dir, web_dir
  defaults/
    user.toml                       # display_name, timezone, template, nightly_time,
                                    # show_arc_between_sessions, counter,
                                    # nightly_enabled, checkins_enabled
    templates/
      default.toml                  # default event template
  users/
    <username>/
      user.toml                     # per-user overrides, merged over defaults
                                    # (incl. ntfy_topic, set from Settings)
      templates/
        <name>.toml                 # per-user template overrides
```

Per-user files are optional; anything not overridden falls back to the
`defaults/` tree. `server.toml` also takes an optional `[limits]` section
(`agent_sessions_per_day`, see "Sessions & limits").

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
    "category": "member", "nightly_enabled": true, "checkins_enabled": true,
    "ntfy_topic": "note-aki", "ntfy_enabled": true,
    "templates": ["default", "deep-work"],
    "timezones": ["Africa/Abidjan", "…"]
  }
  ```

  `templates` is every `.toml` stem under `defaults/templates/` plus the user's
  own `templates/`; `timezones` is the bundled IANA database.

  `ntfy_topic` is the topic the ntfy channel delivers to (see "ntfy") and
  `ntfy_enabled` says whether that channel is configured at all.

- `PUT /api/settings` takes any subset of `display_name`, `timezone`,
  `nightly_time`, `template`, `show_arc_between_sessions`, `counter`,
  `nightly_enabled`, `checkins_enabled` and `ntfy_topic`, and returns the
  merged settings without the two lists. `category` is read-only here — it belongs to the
  account, not the settings file. A rejected field is a
  `400` whose `{"error": …}` names it and leaves the file untouched:
  `display_name` is trimmed, non-blank and at most 64 characters; `timezone`
  must be an IANA name; `nightly_time` must be a zero-padded 24-hour `HH:MM`;
  `template` must be one of `templates`; `show_arc_between_sessions` is a bool;
  `counter` must be `remaining` or `elapsed`; the two feature toggles are
  bools. `ntfy_topic` is the exception to the `400`: an invalid topic is a
  `422`, and an empty string clears the override rather than setting one. The write replaces the user file through a temp file and a rename, so
  a crash mid-write cannot leave a half-written config. A toggle the user has
  never set stays out of the file and keeps following its category default.

### Categories & background features

Every account has a `category` beside its role: `member`, or `test` for one
that exists to exercise the API. The category decides where the two background
features start — the nightly run (the day's plan and its debrief letter) and
check-ins (the scheduled events that reach the user). Both spend model tokens
or attention with nobody asking for them, so a `test` account starts with both
off and a `member` with both on.

`nightly_enabled` and `checkins_enabled` in `user.toml` override that default
either way, and the settings API writes them live: a disabled user is skipped
by the nightly sweep and the delivery sweep without a model call or a log row,
and switching one back on takes effect on the next sweep. Talk is never gated
— an account the user is typing to answers whatever its category.

## Memory & agent tools

Per-user long-term memory lives under `data/memory/<user>/{semantic,episodic,procedural,archive}/` —
one markdown fact per file, frontmatter with a one-line summary. Facts are
never deleted: superseding a fact writes a replacement and moves the old file
to `archive/`. A SQLite FTS index over these files is derived and rebuilt at
startup, so the files themselves are the backup-worthy source of truth.

The standing context document each agent session sees is
`config/users/<user>/standing.md`; agents edit it in place through the
`context_edit` tool, so its history is whatever your config dir's VCS says. The
whole file is prepended to every system prompt, so it is capped at 64 KiB: an
edit that would cross that is rejected and the file is left alone.

Model-facing capabilities are typed tool calls dispatched through a
per-session-type registry (check-in < talk < nightly, with a one-tool import
surface of its own beside them). Every call is validated, size-capped, and
transactional; failures return typed rejections to the model and never leave
partial state.

Tasks: `task_create`, `task_update` (title, description, state, notes,
duration, Now), `task_split` into steps, and `task_delete`, which removes a
task or a single step with its steps and event links exactly as
`DELETE /api/tasks/{id}` does. `task_brief` is the import session's only tool
(see "Briefing an imported task"). Memory: `memory_query`, `memory_read`,
`memory_write`. The day: `schedule_slide`, `schedule_snooze`, `schedule_drop`,
`schedule_reshape`, and — nightly only — `schedule_insert` and `notify_send`.
`context_edit` maintains the standing document.

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
lives in config files. One chat call is capped at `timeout_secs` (default 45),
which keeps a session's calls inside the 100 seconds a tunnel in front of the
server allows a response to take.

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
a deploy. The editable prompts are also served
over `GET /api/prompts/{name}` (`{name, content, custom}`, `content` being the
effective text), `PUT /api/prompts/{name} {content}` (writes the user's
override), and `DELETE /api/prompts/{name}` (drops it, back to the default);
any other name is a 404. A third, `import`, drives the task-briefing route
(see "Briefing an imported task").

Every night at each user's `nightly_time` (default 03:00, their timezone),
the server generates the day's plan from their template, lets the agent
adjust it and write a morning debrief, and stores the debrief. If the model
is unreachable, the plan still exists and a fallback debrief says so — a
plainer day, never a missing one.

## Channels & delivery

When an event fires, the server walks a delivery ladder: connected WebSocket
clients first, then Web Push, then ntfy. The first channel that accepts the
message wins; if none does, the day is plainer, never an error.

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
half-open socket stops absorbing deliveries and the ladder falls through to the
channels below it.

An upgrade whose `Sec-Fetch-Site` says a foreign site started it is refused
(`403`), and an account holds at most 8 sockets at once — a ninth is a `429`
before the upgrade.

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
Without this section and without `[channels.ntfy]`, only in-app WebSocket
delivery is active and every event for a disconnected user is logged as
`delivery_degraded`.

Subscription routes — all cookie-authenticated, the public-key route included:

- `GET /api/push/vapid_public_key` → `{"key": "<base64url>"}`, or `404` when
  Web Push is not configured. Pass the key to
  `pushManager.subscribe({ userVisibleOnly: true, applicationServerKey })`.
- `POST /api/push/subscribe` — the browser's `PushSubscription.toJSON()` shape:

  ```json
  { "endpoint": "https://push.example.net/send/abc", "keys": { "p256dh": "…", "auth": "…" } }
  ```

  Subscribing again with the same endpoint replaces the stored keys. Fields
  over 2048 (`endpoint`) / 256 (`p256dh`) / 64 (`auth`) characters are a `400`.
  The server is the one that posts to an endpoint, so the endpoint must be
  `https://` and its host must not be — or resolve to — a loopback, RFC1918,
  link-local, CGNAT, unique-local or unspecified address, `localhost` included;
  a host that does not resolve is refused as well. Each of those is a `422`
  `{"error": …}`. An account holds at most 10 subscriptions (`409`), and an
  endpoint another account registered is a `409` rather than a change of owner.
- `POST /api/push/unsubscribe {endpoint}` — `404` if the endpoint is not one of
  the caller's.

Payloads are encrypted (`aes128gcm`) and VAPID-signed. Endpoints the push
service reports gone (404/410) are pruned automatically.

### ntfy

An [ntfy](https://ntfy.sh) server delivers to its own desktop and phone clients
over plain HTTP — the way to reach a device from an instance served over
`http://`, where browsers refuse the service worker Web Push needs. Point the
channel at one:

```toml
[channels.ntfy]
base_url = "http://10.0.0.1:2586"
# token_file = "config/ntfy.token"   # bearer token, for a server that needs one
# topic_prefix = "note-"             # the default
```

`base_url` is required; without the section the channel is not built. A
configured `token_file` that is missing or blank fails startup rather than the
first delivery, like a provider's `api_key_file`.

Each user has a topic: `ntfy_topic` in their `user.toml`, or
`<topic_prefix><username>` when they have not set one. A topic is 1 to 64
characters of letters, digits, `_` or `-`, and may not be another account's
default topic (`<topic_prefix><their username>`) — Settings answers `422`.
Subscribing to the topic in an ntfy client is all a device needs.

A delivery is a `POST {base_url}` carrying ntfy's JSON publish form: `topic`,
`title`, `message`, `tags: ["bell"]`, `click` (the instance's
`public_base_url`) and `priority` — 2 for a low-urgency message, 3 for normal,
5 for a check-in — plus `Authorization: Bearer` when a token is configured. The
JSON form rather than the header form because a title carries the event's own
words and an HTTP header value cannot hold them. Anything but a 2xx, and any
transport error, falls through the ladder as `delivery_degraded`.

`POST /api/notify/test` (session cookie) walks the same ladder with a stand-in
message and answers `{"via": "<channel>"}`, or `502` `{"error": …}` when no
channel could take it. Settings offers it as "Send test".

### Delivery in the admin log

Every outcome lands in `event_log`, readable at `GET /api/admin/log`:

- `delivery_ok` — `event <id> via ws`, `via webpush` or `via ntfy`; a
  `POST /api/notify/test` logs the same row as `test via <channel>`.
- `delivery_degraded` — no channel could reach the user; the detail carries
  each channel's reason, or `no channels configured`.
- `voice_unavailable` — a `voice` event fell back to the delivery ladder
  (WebSocket first, then Web Push, then ntfy).

### Sessions & limits

- `POST /api/login {username, password}` sets an HttpOnly, SameSite=Lax session
  cookie valid 30 days (`Secure` whenever `public_base_url` is `https://`).
  `POST /api/logout` deletes the session row and clears the cookie.
- Four password verifications run at once server-wide; a login that arrives
  while all four are busy is a `503` with `Retry-After: 2`. Hashing is what the
  route costs, so that is what bounds a flood of fresh usernames.
- A failed sign-in counts against 10 attempts per username per 15-minute window,
  and the eleventh failure is a `429`. The counter is consulted only after a
  verification has already failed, so the right password always signs in and
  wrong guesses cannot lock an account's owner out. A successful login clears it.
- `POST /api/talk` runs one session per user (a second concurrent request gets
  `409`) and four across the server (`503` with `Retry-After: 5` beyond that).
  `POST /api/tasks/{id}/agent` takes the same gate: a second session for the
  same user is a `429`, the server-wide cap a `503`.
- One account may start `[limits] agent_sessions_per_day` agent sessions in any
  24 hours — default 200, `0` lifts the ceiling — and beyond that both routes
  answer `429` `{"error": "daily session limit reached"}`. The count comes from
  the `agent_session` rows already in `event_log`, and a session an API token
  started names it (`token=<id>`) so the spend is attributable.

### API tokens

A user can mint long-lived bearer tokens for scripts and other agents. Tokens
reach the task routes only; every other route still needs the session cookie.

- Mint one in Settings → API tokens, or over the session:
  `POST /api/tokens {name}` → `{id, name, created_at, last_used_at, token}`.
  The `token` (`note_…`) is shown once; only its SHA-256 digest is stored.
  `GET /api/tokens` lists `{id, name, created_at, last_used_at}`;
  `DELETE /api/tokens/{id}` revokes. Names are 1 to 64 characters (`422`),
  and a user holds at most 20 tokens (`409`).
- Send it as `Authorization: Bearer note_…` on `GET/POST /api/tasks`,
  `PATCH/DELETE /api/tasks/{id}`, `POST /api/tasks/{id}/split`, and
  `POST /api/tasks/{id}/flatten`. A bearer header that does not resolve is
  `401` even when a session cookie is also present. Disabling the user stops
  their tokens.
- `DELETE /api/tasks/{id}` removes the task, its steps, and their event links
  (`204`, or `404` when the task is not the caller's).
- Minting and revoking log `token_created` / `token_revoked` to `event_log`.

### Briefing an imported task

An importer that mirrors outside work into Note — school assignments, say —
can hand a task to Note's own agent instead of running a model of its own:

```sh
curl -X POST http://localhost:3271/api/tasks/42/agent \
  -H 'Authorization: Bearer note_…' -H 'Content-Type: application/json' \
  -d '{"context":"Due Friday. Worksheet handed out in class."}'
```

`context` is optional (`{}` and an empty body mean "no context") and is
capped at 32 KiB. The reply is the session and its result:

```json
{
  "task_id": 42,
  "outcome": "briefed",
  "steps": [{ "name": "task_brief", "args": "…", "result": "…", "is_error": false }],
  "task": { "id": 42, "description": "…", "children": [] }
}
```

`outcome` is `dropped` when the task ends the session dropped, `unchanged`
when no tool call of the session succeeded, and `briefed` otherwise. `task` is
the same shape as one entry of `GET /api/tasks`, steps included. The failures
are `404` (unknown task, or not the caller's), `409` (the id is a step — only
top-level tasks are briefed), `422` (context over 32 KiB), `429` (a session for
that user is already running, or the daily ceiling is reached), `502` (the model
is unreachable) and `500`; every one of them carries a `{"error": …}` body, and
`502`/`500` also write a `task_agent_error` row to `event_log`.

The session is scoped: it is a fresh run with no conversation history, nothing
is kept in the talk conversations, and its only tool is `task_brief`, aimed at
that one task:

```json
{ "task_id": 42, "homework": true,
  "description": "Read chapter 4 and answer the questions.\nHand in: worksheet in class.",
  "duration_min": 45,
  "steps": [{ "title": "read chapter 4", "duration_min": 25 },
            { "title": "answer the questions", "duration_min": 20 }] }
```

It answers `{"task_id", "outcome": "briefed"|"dropped", "steps_applied"}`,
where `steps_applied` is the number of steps written, `0`, or `"kept"` for a
task that already had steps. `homework: false` is the other call: it drops the
task and writes `"Not homework: <reason>"` as the description, the reason being
required and at most 100 characters; everything else in the call is ignored.
The whole brief is one transaction — a rejected field leaves nothing behind.

One brief is one model round: a successful `task_brief` ends the session on the
spot, and a rejected one is fed back for a single retry, so a session costs at
most two calls to the provider. It cannot move anything into Now, it can drop
only a task that is still open — one already `in_progress` or `done` is
briefed, never dropped — and it cannot change a state to anything but
`dropped`. A call that breaks those rules comes back in `steps` as a rejection;
only a failed session rolls back, and then the task, its steps and their event
links are restored exactly as they were, so the same id can simply be retried.

Field ownership splits cleanly: the agent owns the `description`, the
`duration_min` (written with `duration_source: "agent"`), the steps and the
`dropped` state; the caller owns the `title` and the `notes`, which the agent
is told never to touch. Re-briefing a changed task is safe — existing steps are
left alone, since the user may already have ticked some off, and the agent
updates the description and duration only (the call answers
`"steps_applied": "kept"`). It says so in the description when the new text
really invalidates those steps. Call
`POST /api/tasks/{id}/flatten` first only when you do want the steps regenerated.

What the agent is told lives in `config/defaults/prompts/import.md`, editable
per user like the other prompts over `GET/PUT/DELETE /api/prompts/import`.

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
(template, which routines ping, whether the nightly run happens at all and
when, timezone), whether check-ins reach you, your name, a
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
2. an *admin grant*: the panel asks for the password again plus a second factor
   — a passkey, or a 6-digit code (the password alone under
   `require_second_factor = false`) — and the server answers with a second
   cookie (`admin=…; Path=/api/admin; HttpOnly; SameSite=Strict`, `Secure` on
   HTTPS) valid 15 minutes, bound to that session, and deleted with it. Every
   route except `gate`, `elevate` and `drop` answers `401` without a live grant;
3. same-origin provenance: a request whose `Sec-Fetch-Site` says a foreign site
   started it is refused (`403`).

Elevation attempts are limited like logins (10 per username per 15 minutes),
TOTP codes and passkey assertions are single-use, and admin responses are
`Cache-Control: no-store`.

### Second factors

Every account enrols its own in Settings → SECURITY, admin or not:

- **Passkeys.** "Add a passkey" re-asks for the password, then hands the
  browser a WebAuthn challenge; the credential is named and stored. An account
  holds at most 10. Rename and remove are on the row (removal re-asks for the
  password). Browsers only speak WebAuthn to a secure origin, so this needs
  `public_base_url` on `https://` (or `http://localhost` for development);
  anywhere else the section says so and the button stays off.
- **Authenticator app.** "Set up authenticator" shows a QR code (rendered in
  the page — no image service ever sees the secret), the `otpauth://` URI as a
  link a password manager can take, and the base32 key for the manual field.
  A code from the app finishes enrolment; until then the secret sits in
  `users.totp_pending` and a later setup replaces it.

Secrets are stored as base32 text in `users.totp_secret`, not encrypted: the
database file is the trust boundary, and anything reading it already holds the
session table and the argon2 hashes. Back it up accordingly.

Enrolment routes, all on the session cookie. Asking for a challenge and
dropping a factor re-check the account password; finishing a registration rides
on the challenge that password bought:

| Route | Body | Effect |
|---|---|---|
| `GET /api/security` | — | `{passkeys: [{id, name, created_at, last_used_at}], totp: {enabled, pending}, webauthn_available}` |
| `POST /api/security/passkeys/challenge` | `{password}` | WebAuthn creation options; `503` where passkeys can't run, `409` at the cap |
| `POST /api/security/passkeys` | `{name, credential}` | `{id, name, created_at, last_used_at}`; `422` bad name or challenge, `409` cap or a credential already registered |
| `PATCH /api/security/passkeys/{id}` | `{name}` | the renamed row; `422` invalid, `404` not yours |
| `DELETE /api/security/passkeys/{id}` | `{password}` | `204`; `404` not yours |
| `POST /api/security/totp/start` | `{password}` | `{secret_base32, otpauth_uri, issuer, account}` — the only response carrying the secret |
| `POST /api/security/totp/confirm` | `{code}` | `204`; `401` when it doesn't match |
| `DELETE /api/security/totp` | `{password}` | `204`, clearing the secret and any pending one |

A wrong password is `401` and changes nothing; ten wrong ones in 15 minutes are
`429`. Adding and dropping factors write `passkey_added`, `passkey_removed`,
`totp_enrolled` and `totp_removed` to `event_log`.

The relying party is the host of `public_base_url`, named "Note". A deployment
whose browsers see a different origin overrides both:

```toml
[admin]
rp_id = "note.example.net"
rp_origin = "https://note.example.net"
```

### Legacy: the shared admin seed

`secrets_dir` in `server.toml` (default `persist/secrets`, gitignored) may hold
`admin_totp`: one base32 TOTP seed (SHA-1, 6 digits, 30 s) shared by every
admin. It answers for an admin who has enrolled nothing of their own, and stops
being consulted for one who has.

```sh
note-server totp-generate            # prints a seed and its otpauth:// URI
note-server totp-uri                 # the URI for the seed already installed
```

With no seed installed and nothing enrolled, a release server keeps that
account out: the gate reports `totp: "missing"` and elevation answers `503`
(enrolment itself needs no elevation, so Settings is the way in). Startup
writes an `admin_locked` row so the state is visible in the log.

### Password-only elevation

An operator who does not want a second factor opts out in `server.toml`:

```toml
[admin]
require_second_factor = false        # require_totp still reads as its old name
```

Elevation then re-asks for the account password alone — same grant cookie,
same 15 minutes, same limiter and audit rows — and no factor is consulted,
enrolled or not. The gate reports `require_second_factor: false` and the panel
drops the second field. The default is `true`.

### Dev builds

```sh
cargo run --features dev-inspect
```

`dev-inspect` is a Cargo feature, off by default and off in the Nix package.
It compiles in the inspection routes (`/api/admin/inspect/...`: a user's
config, tasks, today's events, conversations and memory files, all editable,
plus a SQL console on the live database), lets elevation pass on the password
alone for an account with no factor and no seed, and makes the panel show a red
banner. The
web client is a single build; it renders the inspection section only when the
server's gate reports `inspect: true`.

### Routes

| Route | Needs | Effect |
|---|---|---|
| `GET /api/admin/gate` | role | `{elevated, expires_at?, second_factor, methods: {passkey, totp}, require_second_factor, totp, inspect}`; `second_factor` is what to ask this admin for (`passkey`, `totp`, `none`), `methods` what they hold, and `totp` the former report (`required`, `password_only`, `missing`) kept for one release |
| `POST /api/admin/elevate/challenge` | role | WebAuthn request options for this admin's passkeys; `409` when they have none, `503` where passkeys can't run |
| `POST /api/admin/elevate` `{password, code?, assertion?}` | role | sets the grant cookie; the password plus a passkey assertion, a code from the account's app, or the shared seed's code |
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
