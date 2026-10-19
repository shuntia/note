# Delivery

When an event fires, the server walks a ladder: connected WebSocket clients,
then Web Push. The first channel that accepts wins; if none does, the day is
plainer, never an error. Matrix (with `[channels.matrix]`) sits first as a
companion: it posts a copy when the user has `matrix_send` on and the walk goes
on. The ladder never rings; only [a call](#ringing) does.

Every outcome lands in `event_log` (readable at `GET /api/admin/log`):

- `delivery_ok`: `event <id> via <channel>`; `POST /api/notify/test` logs
  `test via <channel>`.
- `delivery_degraded`: no channel reached the user; the detail carries each
  channel's reason, or `no channels configured`.
- `delivery_deferred`: held by a quiet window ([planning.md](planning.md#quiet-windows)).

`POST /api/notify/test` (session cookie) walks the ladder with a stand-in
message and answers `{"via": "<channel>"}`, or `502` `{"error"}` when no
channel took it. Settings offers it as "Send test".

## WebSocket

`GET /api/ws` upgrades to a WebSocket for the session's user (cookie auth) and
receives one JSON frame per delivery:

```json
{ "type": "event", "title": "Check-in", "body": "checkin at 09:00", "urgency": "high", "event_id": 7,
  "conversation_id": 12 }
```

`conversation_id` is the thread a check-in opened, `null` otherwise; the web
client switches to it, and push notifications deep-link to `/#/chat/<id>`.
`{"type":"changed"}` tells the client the user's day changed under it (a
wake-up's edit, a reordered day) so it refetches; `incoming` and `ring_taken`
belong to [ringing](#ringing).

A talk session in flight streams its progress to the same sockets:

```json
{ "type": "agent", "conversation_id": 12, "seq": 3,
  "event": { "kind": "tool_call", "index": 1, "name": "memory_query", "args": "{\"query\":\"groceries\"}" } }
```

`seq` counts from zero within a session. `conversation_id` is `null` until a
new conversation's reply hands the client its id. `event.kind` is `thinking`
(`text`), `tool_call` (`index`, `name`, `args`), `tool_result` (`index`,
`name`, `result`, `is_error`), `reply` (`text`) or `error` (`message`); every
variable field is clipped to 4 KiB. Only `POST /api/talk` emits these.

Inbound frames count as presence. A page reports whether it is in view with
`{"type":"visible","on":bool}` and declines a ring with
`{"type":"decline","ring"}`; anything else is ignored. The server pings every
30 s and drops a connection silent for 90 s, so a half-open socket stops
absorbing deliveries.

An upgrade is `403` when `Sec-Fetch-Site` names another site, or when an
`Origin` is sent that is neither one of Note's page origins (`public_base_url`'s,
and the listening address's, with its loopback names when it listens on
loopback or every address) nor the request's own `Host` over https (plain http
only for a loopback host or the listening address). An account holds at most 8
sockets (a ninth is `429` before the upgrade).

## Ringing

Three things ring, all through one gate (`call_ops::gate`): a confirmed
`call_user` ([agent.md](agent.md#wake-ups)), a trigger laid with
`trigger_set {ring: true}` once its session `say`s, and a template routine with
`channel = "call"`. The gate holds a call when nothing can ring, a call is
live, the user's `ring_for` is `never`, or the user seems busy (a quiet window,
a `fixed` or `busy` calendar occurrence, an open work session, or outside
`day_start` to `close_day_time`). Only `call_user` with the user's own words
asking for the call gets past the busy part. A held ring sends the message
down the ladder and logs `ring_held` with why.

When the gate lets it through, Note calls instead of writing:

The open app and the linked phone ring together; whichever is answered first
takes the call and the other stops.

- With `[voice]` up and a page of the app in view, every visible socket gets
  `{"type":"incoming","ring","conversation_id"}` and the app shows the call
  view, ringing, with a soft repeating ringtone (a browser tab may block it
  until the page has had a click). The desktop app always reports itself in
  view, so it rings from the tray and brings its window up. Answering opens
  `/api/call/ws?ring=<token>` ([voice.md](voice.md#web-calls)), hangs up the
  phone if it is still ringing, and every socket gets
  `{"type":"ring_taken","ring"}`.
- With a linked phone, it rings over Matrix at the same time. Answering it
  sends every socket `{"type":"ring_taken","ring"}` for the app's ring.
- Declining in the app hangs up the phone and sends the message down the
  ladder at once. When neither is answered, the message goes down the ladder
  once both have given up: after the phone's ring when it rang, else after the
  app's 30 s. A ring answered but unable to open its call goes down the ladder
  too.
- With neither, the message alone walks the ladder without the voice rung.

An answered call sounds when it connects and when it ends; a failed call and a
ring that is never answered stay quiet. The sound files and their licences are
in `web/src/sounds/` (`CREDITS.md`).

Each ring a wake-up makes logs `trigger_rang` with `web+phone`, `web`, `phone`,
`messaged` or `undelivered`.

## Check-ins are conversations

A check-in's question (the event's `message`, or the rendered "Time for your
09:00 check-in" line) is written to a conversation as an assistant message
before any channel carries it. One thread per plan date: the first check-in of
a day creates it (`checkin_date` marks the row), later ones append, and the next
day starts fresh. The thread exists even when no channel delivered. Replying
goes through `POST /api/talk` with that `conversation_id`; the session runs on
the talk surface with a `# This conversation` note saying the opening turns were
scheduled check-ins.

## Web Push

Needs a VAPID keypair (any P-256 EC key):

```sh
openssl ecparam -genkey -name prime256v1 -noout -out config/vapid.pem
```

```toml
[channels.webpush]
vapid_pem_file = "config/vapid.pem"
subject = "mailto:admin@example.com"
```

`subject` must be a `mailto:` or `https://` URL; an unreadable key or bad
subject fails startup.

Routes, all cookie-authenticated:

- `GET /api/push/vapid_public_key` → `{"key": "<base64url>"}`, or `404` when
  Web Push is off. Pass it to
  `pushManager.subscribe({ userVisibleOnly: true, applicationServerKey })`.
- `POST /api/push/subscribe`: `PushSubscription.toJSON()`,
  `{ "endpoint": "https://…", "keys": { "p256dh": "…", "auth": "…" } }`. The
  same endpoint again replaces the keys. Fields over 2048 (`endpoint`) / 256
  (`p256dh`) / 64 (`auth`) characters are `400`. The endpoint must be
  `https://` and its host must not be or resolve to a loopback, RFC1918,
  link-local, CGNAT, unique-local or unspecified address (`localhost`
  included), nor fail to resolve: each is `422`. At most 10 subscriptions per
  account (`409`); an endpoint another account registered is `409`.
- `POST /api/push/unsubscribe {endpoint}`: `404` if not the caller's.

Payloads are `aes128gcm`-encrypted and VAPID-signed; the plaintext is
`{"title", "body"}` plus `conversation_id` and `url` (`/#/chat/<id>`) when the
event opened a thread, and `actions` (`[{action, title}]`) when the message
carries buttons. `web/public/sw.js` shows the notification and focuses or
opens the app on click. Endpoints the push service reports gone (404/410) are
pruned.

## Buttons

A message can carry buttons: a check-in's Done, Snooze 15 and Drop
(`ev:done|snooze|drop:<event>`), a spoken trigger's Snooze 15, the close of the
day's Carry to tomorrow (`carry:<date>`), and a block's Start session
(`block:start:<event>`). Each delivery keeps its buttons in `message_actions`
under keys of its own for 36 hours; pressing one spends them all.

- Web Push: each button's `action` is a random 128-bit key. The service worker
  posts `POST /api/actions {key}`, with no session needed: the key is the
  credential. `204` when done, `404` for a key spent, expired or never given,
  `409` when the thing has already settled. A failed press opens the app.
- Matrix: the bot posts a legend line (`✅ Done · 💤 Snooze 15 · ✖ Drop`) and
  reacts to its own message with each emoji; the linked account tapping one
  presses it, and the bot answers with a short notice.

Snooze counts from now, not from the event's own time. Snoozing a trigger that
spoke asks it again. Starting a block opens a work session on its task for the
block's length.

## Outages

When something Note leans on fails for a user, it stands as an outage in
`outages` until it clears: the AI service failing a session (model gone, key
refused, out of credits, rate limited, unreachable) until a later session gets
through; the day's `agent_sessions_per_day` reached, until it lifts; Matrix
refusing a post, until one gets through; the voice service failing a ring,
until one starts. Each new outage is told once, down the ladder, by the next
sweep (at most once per reason in 6 hours). `GET /api/outages` →
`{"outages": [{"code", "since"}]}` feeds the app's banner, which stays until the
list is empty.

Past the day's cap a wake-up runs no session: one laid by the user or by Note
for later (`agent`, `template`) goes out as `Reminder: <its prompt>` (the close
of the day as a canned line with its carry button), and Note's own looking
(idle, laying the day, a resumed wait and its parked session) settles without a
word.

## Matrix

```toml
[channels.matrix]
homeserver = "https://matrix.example.org"
token_file = "config/matrix.token"   # mode 600, gitignored
```

The bot account's DM is one more window onto the same conversations, and the
room calls ring in. The token is the bot's own login (its own device), not the
voice service's. Users toggle it with `matrix_send` and `matrix_ping` in
settings.
