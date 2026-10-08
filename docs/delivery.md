# Delivery

When an event fires, the server walks a ladder: a voice ring of the linked
phone (with `[voice]`, a linked Matrix account and a message the user's
`ring_for` covers), connected WebSocket clients, then Web Push. The first
channel that accepts wins; if none does, the day is plainer, never an error.
Matrix (with `[channels.matrix]`) sits after the voice ring as a companion: it
posts a copy when the user has `matrix_send` on and the walk goes on.

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

A wake-up's `say {ring: true}` ([agent.md](agent.md#wake-ups)) calls instead of
writing, whatever `ring_for` says:

1. With `[voice]` up and a page of the app in view, every visible socket gets
   `{"type":"incoming","ring","conversation_id"}` and the app shows the call
   view, ringing, with a soft repeating ringtone (a browser tab may block it
   until the page has had a click). The desktop app always reports itself in
   view, so it rings from the tray and brings its window up. Answering opens `/api/call/ws?ring=<token>`
   ([voice.md](voice.md#web-calls)) and every socket gets
   `{"type":"ring_taken","ring"}`; so does declining, which sends the message
   down the ladder at once. A ring unanswered after 30 s, or answered but
   unable to open its call, goes down the ladder too.
2. Otherwise the linked phone rings over Matrix.
3. Otherwise the message alone walks the ladder without the voice rung.

An answered call sounds when it connects and when it ends; a failed call and a
ring that is never answered stay quiet. The sound files and their licences are
in `web/src/sounds/` (`CREDITS.md`).

Each ring logs `trigger_rang` with `web`, `phone`, `messaged` or
`undelivered`.

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
event opened a thread. `web/public/sw.js` shows the notification and focuses or
opens the app on click. Endpoints the push service reports gone (404/410) are
pruned.

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
