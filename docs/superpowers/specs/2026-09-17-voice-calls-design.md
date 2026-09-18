# Accountability calls

Date: 2026-09-17. Status: approved for implementation (autonomous session).

## Goal

Note can ring the user's phone. A scheduled event whose channel is `voice`
places a real call through Twilio: Note says what the check-in is about and
the user answers on the keypad (done / snooze / drop). The agent and the user
can ask for a call on any event. No speech-to-speech bridge in this release.

## Configuration

`server.toml`:

```toml
[channels.voice]
account_sid = "AC…"
auth_token_file = "config/twilio.token"   # mode 600, gitignored
from_number = "+1…"                        # E.164
# base_url = "https://api.twilio.com"      # tests point this at a local one-shot server
```

`VoiceSettings` under `ChannelsConfig.voice: Option<VoiceSettings>`; the
constructor fails boot on a blank sid/number or an unreadable token file, the
way ntfy does. `public_base_url` is handed in at construction and must be
https (otherwise boot fails with a clear message: Twilio will not fetch
plain-http TwiML from a public host).

`user.toml` / settings API: `phone_number: Option<String>` (E.164, validated
`^\+[1-9]\d{6,14}$`, blank clears) and `calls_enabled: Option<bool>`. Settings
body exposes `voice_enabled` (server has the channel), `phone_number`,
`calls_enabled` (resolved: default true for members, false for test
accounts via `Features`). `POST /api/notify/call` places a test call with the
message "This is Note. Your phone is set up." → `{ "call_id" }` or 502 with
`{ "error" }`; 409 when the user has no phone number.

## Data model (migration appended)

```sql
CREATE TABLE voice_calls (
    id INTEGER PRIMARY KEY,
    token TEXT NOT NULL UNIQUE,          -- 32 random bytes, base64url; the only handle Twilio sees
    user_id INTEGER NOT NULL REFERENCES users(id),
    event_id INTEGER REFERENCES events(id),
    message TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'placed'
        CHECK (status IN ('placed','ringing','answered','completed','no_answer','busy','failed')),
    digit TEXT,                          -- what the user pressed, if anything
    call_sid TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_voice_calls_user ON voice_calls(user_id, created_at DESC);
```

## The channel — `server/src/channels/voice.rs`

`VoiceChannel` implements `Channel` (`name() = "voice"`). `deliver` loads the
user's config; without a phone number or with `calls_enabled` off it returns
`Err("no phone")` so the caller falls through. Otherwise it inserts a
`voice_calls` row, then POSTs `.../2010-04-01/Accounts/{sid}/Calls.json`
(basic auth sid:token, form body `To`, `From`, `Url =
{public}/api/voice/twiml/{token}`, `StatusCallback = {public}/api/voice/status/{token}`,
`StatusCallbackEvent = answered completed`, `Timeout = 25`, `MachineDetection`
off) with 5 s / 15 s timeouts. 201 → store `call_sid`, `Ok(())`. Anything
else → mark the row `failed` with the reason and `Err`. Like ntfy, the DB
guard is not held across the HTTP call. The channel is **not** in the generic
ladder: `channels::deliver_event` tries it first only when `ev.channel ==
"voice"`, and on `Err` logs `voice_fallback` and continues down the push
ladder (replacing today's `voice_unavailable` row).

## Inbound routes (no cookie; Twilio signature instead)

Mounted under `/api/voice`, all `POST`, each validating `X-Twilio-Signature`
(HMAC-SHA1 over the full public URL + sorted form params, base64; the crates
are already dependencies) against the configured token and returning 403 on
mismatch; 404 for an unknown or expired (>2 h old) token.

- `/twiml/{token}` → `application/xml`:
  ```xml
  <Response>
    <Gather numDigits="1" timeout="8" action="/api/voice/gather/{token}">
      <Say>Hi {display_name}. This is Note. {message}
           Press 1 if it is done, 2 to snooze it for fifteen minutes, 3 to drop it.</Say>
    </Gather>
    <Say>No answer taken. I will check in again later.</Say>
  </Response>
  ```
  For a call with no event (test call) the Gather is omitted and the reply is a
  single `<Say>`. Text is XML-escaped.
- `/gather/{token}` with `Digits`: `1` → `plan::set_status(done)`, `2` →
  `plan::snooze(15)`, `3` → `plan::set_status(dropped)`; store `digit`, mark
  `answered`; reply `<Say>Got it.</Say><Hangup/>`; an unrecognised digit
  replays the Gather once. Every applied decision logs `voice_decision`
  (`"event {id}: done|snoozed|dropped by phone"`) and is visible to the client
  through the normal refresh path (the ws hub broadcasts a `changed` frame:
  `EventFrame` gains no new type; the route calls `state.hub.broadcast_changed(user_id)`
  which emits `{ "type": "changed" }` and the client treats it like any event
  frame's `onChanged`).
- `/status/{token}` with `CallStatus`: maps `ringing|in-progress|completed|no-answer|busy|failed|canceled`
  onto the row; `no-answer`/`busy`/`failed` log `voice_unanswered` and, when
  the call carried an event, deliver the same `OutboundMessage` through the
  push ladder so the nudge still lands.

## Reaching for a call

- Template entries already accept `channel = "voice"`.
- `POST /api/events/{id}/channel { "channel": "push" | "voice" }` (CurrentUser;
  422 on a decided event or an unknown channel). The event overflow menu gains
  "Call me" / "Notify me" toggling it; the row shows a phone glyph when the
  channel is voice.
- `notify_send` gains `channel: Option<"push"|"voice">` (default push) so the
  agent can escalate a nudge to a call; its description names the cost ("rings
  the phone; only when the user asked for calls").
- Settings → a "Calls" card under Notifications: phone number field,
  "Call me for check-ins" switch, "Call me now" test button. Hidden when the
  server has no voice channel.

README: a "Calls" section (config, per-user fields, the three routes, that
quiet windows defer calls exactly like pushes because the runner does).

## Testing

`channels/voice.rs`: one-shot TCP server pinning the wire (basic auth, form
fields, callback URLs), 201 stores sid, 4xx/5xx is `Err` and marks failed, no
phone is `Err` without a network call. `security`-style unit tests for the
signature (a known Twilio example vector plus a tampered body). `tests/voice_api.rs`
drives the three routes through `oneshot` with a computed signature: TwiML
shape and escaping, gather 1/2/3 changes the event, bad digit replays, 403
on a bad signature, 404 on an unknown token, status no-answer falls back to
a `MockChannel`. `delivery_day.rs`'s voice check-in now expects a placed call
against a fake Twilio and a `voice_fallback` row when it is refused.
`settings_api.rs`: phone validation (422), clearing, `voice_enabled`.
