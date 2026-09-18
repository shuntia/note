# Telegram as a chat location

Date: 2026-09-17. Status: spec, awaiting go-ahead.

## Goal

A Telegram bot is one more window onto the same conversations: Note delivers
check-ins and replies there, the user answers there, and everything lands in
the same conversation tables the web chat, the idle summaries and the nightly
harvest already read. No separate history, no sync job.

## Configuration

```toml
[channels.telegram]
token_file = "config/telegram.token"       # mode 600, gitignored
# base_url = "https://api.telegram.org"    # tests point this at a one-shot server
```

`TelegramSettings` under `ChannelsConfig.telegram: Option<_>`. Boot calls
`getMe` once to learn the bot username (failure = boot error naming the token
file). `AppState.telegram: Option<Arc<TelegramChannel>>`.

## Data model (one migration)

```sql
CREATE TABLE telegram_links (
    user_id INTEGER PRIMARY KEY REFERENCES users(id),
    chat_id INTEGER NOT NULL UNIQUE,
    handle TEXT NOT NULL DEFAULT '',
    linked_at TEXT NOT NULL
);
CREATE TABLE telegram_link_codes (
    code TEXT PRIMARY KEY,                 -- 6 chars, A-Z2-9
    user_id INTEGER NOT NULL REFERENCES users(id),
    expires_at TEXT NOT NULL               -- 10 minutes
);
CREATE TABLE telegram_cursor (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    last_update_id INTEGER NOT NULL
);
ALTER TABLE conversations ADD COLUMN via TEXT NOT NULL DEFAULT 'web'
    CHECK (via IN ('web','telegram'));     -- where the user last spoke from
ALTER TABLE conversations ADD COLUMN telegram_at TEXT;   -- last time this thread crossed Telegram
```

## Linking

- `POST /api/telegram/link` (CurrentUser) → `{ code, bot, url }` where `url`
  is `https://t.me/<bot>?start=<code>`; one live code per user (a new call
  replaces it). `DELETE /api/telegram/link` unlinks. Settings body gains
  `telegram_enabled`, `telegram_linked`, `telegram_bot`.
- The inbound loop sees `/start <code>` from an unknown chat: valid code →
  insert the link, delete the code, reply "Linked to Note as <display_name>".
  Anything else from an unlinked chat → "This bot is private. Link it from
  Note's settings." at most once an hour per chat (in-memory map).
- Settings → a "Telegram" card: link button showing the code and the deep
  link as a QR (qrcode is already a dependency), linked state with the bot
  name, unlink.

## Outbound — `channels/telegram.rs`

`TelegramChannel` implements `Channel` (`name() = "telegram"`), placed
**first** in the ladder. `deliver` → `sendMessage { chat_id, text }` for the
linked chat; no link → `Err("not linked")` so the ladder falls through. Text
is `title` on its own line then `body`, plain text, split at 4000 chars.
A message that carries a `conversation_id` (a check-in, a trigger) stamps
the conversation `telegram_at = now`, so the reply routes back to it.

## Inbound — `telegram.rs` loop

Long-polls `getUpdates { offset, timeout: 30, allowed_updates: ["message"] }`
with a 40 s client timeout, persists `last_update_id` after each batch, and
backs off 5 s → 60 s on errors (logged `telegram_error`, throttled). Each
text message from a linked chat:

1. `/new` → the next message opens a fresh conversation; reply "Fresh start."
2. Otherwise pick the conversation: the user's thread with the newest
   `telegram_at` within `idle_summary_min` continues; else a new
   conversation with `via = 'telegram'`, titled from the message.
3. Run exactly what `POST /api/talk` runs: daily cap (over → "You've used
   today's sessions."), `talk_gate` (busy → "Still on your last message."
   once), `spawn_blocking` Talk session with the thread's history and notes,
   persistence in the same order, `touch`, `via = 'telegram'`,
   `telegram_at = now`.
4. Reply with `sendMessage`; a session error → "Couldn't reach Note right
   now." and nothing persisted, as on the web.

The talk route is refactored so both entry points call one
`talk::run_turn(state, user, conversation, message, via)`.

## Mirroring

- Assistant replies always reach the web: the hub already receives agent
  frames; the reply row is in the DB for the drawer.
- A reply produced on the **web** to a thread whose `via = 'telegram'` is
  also sent to Telegram, and `via` flips to `web` when the user's next
  message comes from the web. So Note answers where the user last spoke,
  and the app is always the full record.
- The chat drawer shows a small Telegram glyph on threads with
  `via = 'telegram'`.

## Testing

One-shot TCP fixtures for `getMe`, `sendMessage`, `getUpdates`. Unit:
routing window (continue vs new, `/new`), link code lifecycle (expiry,
replacement, unknown chat throttle), cursor persistence across a restart,
message splitting, mirror rules. Integration: `tests/telegram_api.rs` drives
link/unlink routes and a scripted `getUpdates` batch through a MockLLM
Talk session and asserts the rows, the `sendMessage` wire, and the web
frames.
