# Post-its, inbox, a quieter Now, Matrix messaging, and translation plumbing

Six changes, specified together and built as separate plans. Each group
ships on its own, in this order:

1. The Now face
2. Inbox
3. Post-it reminders
4. Idle nudges
5. Matrix messaging and Connections
6. Translation plumbing

## 1. The Now face

### Stop

- A Stop button appears only while a session is paused. It ends the session
  at once as `stopped` through `POST /api/sessions/{id}/end`.
- It takes the same optimistic path and undo toast as swipe-down.
- A session stopped within its first minute is discarded, as a sideways
  swipe already does (`quick`).

### Candidates for an instant start

- When Now starts a session with no task given, the task strip is built from
  today's scheduled blocks: `events` linked to tasks through `event_tasks`,
  for today's plan, `pending` or `snoozed`, in `wall_time` order.
- The first candidate is the block current or next by the clock.
- When nothing is scheduled today, the strip falls back to the priority
  queue (`GET /api/tasks/queue`).
- A server route returns the candidates as `QueueEntry` rows, so the client
  keeps one shape. Each carries `reason: "scheduled"` and its `planned_min`
  from the block's length.
- Scheduling itself is unchanged: nightly planning and the free-time
  allocator already lay the day's blocks.

### Idle: only the timer

When `html.idle` is set during a session, the following are gone: the round
beads, the task strip and its dots, the hints, the top bar, and the mobile
tab bar and its handle.

- **Gone means removed**, not faded:
  - `visibility: hidden` after a short fade,
  - `pointer-events: none`,
  - the strip cannot be dragged.
- **The timer stays exactly where it is.** Nothing reflows around it.
- **Any wake event brings everything back** with a 200 ms fade.
- **Reduced motion** skips the fades.
- **Outside a session** Today keeps its current idle behaviour.

## 2. Inbox

### What is kept

A new table `inbox_items`:

| Column | Meaning |
|---|---|
| `user_id` | Owner |
| `source_id` | Unique per user |
| `kind` | `announcement` or `material` |
| `title` | First line of the item's context, trimmed to 200 characters |
| `body` | The context as received |
| `received_at` | Time of the most recent arrival |
| `outcome` | `remembered`, `nothing`, `task`, or null while undecided |
| `reason` | Why Note decided as it did |
| `decided_at` | When it decided |

- `POST /api/agent/inbox` upserts the item before its session runs.
- `inbox_decide` records the outcome and reason on it.
- A re-sent item updates the row in place, keeping the one-row-per-source
  behaviour the memory side already has.

### The view

- A section in the Memory tab, newest first. Each row shows the title, a
  chip for the kind, and a chip for the outcome.
- Opening a row shows the body, the reason, and the memories it produced
  (from `memory_sources`), each linking to its memory.
- Routes: `GET /api/inbox?before=<received_at>&limit=` and
  `GET /api/inbox/{id}`.

### Refresh

- `POST /api/inbox/refresh` writes the current time to
  `/run/note/inbox-refresh` and returns `202`.
- The host adds a systemd path unit (`schoolwork-check-refresh.path`,
  `PathModified=/run/note/inbox-refresh`) that starts
  `schoolwork-check.service`. Note gains no privilege.
- The Refresh button shows a spinner until an item arrives newer than the
  press, or 60 s pass, then shows "Up to date".
- The time of the latest arrival stands beside it.
- The file path is configurable as `[inbox] refresh_signal`. With it unset,
  the route answers `409` and the button is hidden.

## 3. Post-it reminders

### The data

A new table `reminders`:

| Column | Meaning |
|---|---|
| `id` | Primary key |
| `user_id` | Owner |
| `text` | One line, 200 characters at most |
| `color` | `yellow`, `pink`, `blue` or `green`, default `yellow` |
| `pinned` | Pinned post-its are never nudged about |
| `created_at` | When it was stuck up |
| `peeled_at` | Null while it is up |
| `last_nudged_at` | Last idle nudge about it |

Peeled post-its are kept 7 days for undo, then deleted by the nightly job.

Routes:

- `GET /api/reminders` returns the open ones plus those peeled in the last
  7 days.
- `POST /api/reminders {text, color?}`.
- `PATCH /api/reminders/{id} {text?, color?, pinned?, peeled?}`.
- `DELETE /api/reminders/{id}`.

### Today's resting face: the board

When no session is running, Today's session face is replaced by a board of
the open post-its.

- **Each note:** a small square in its colour with a slight tilt from a
  stable per-id angle, and the text wrapped to four lines.
- **Adding:** an empty tile at the end. Typing and pressing Enter sticks a
  new note, which lands with a small drop.
- **Peeling:** a swipe up, or a check that appears on hover or focus. The
  note lifts away, and an undo toast brings it back.
- **Menu:** right-click or long-press (the existing `Overflow`) offers edit
  in place, colour, pin and peel.
- **Layout:** the board wraps and centres. At phone width it is a two-column
  grid.
- **Starting a session:** the existing way to start stays (tapping the
  timer's place, or the start control). The board fades out as the session
  face comes in.

### Chat

A new tool group `reminder_*` in the Talk, Check-in and Trigger registries:

- `reminder_add {text, color?}`
- `reminder_update {id, text?, color?, pinned?}`
- `reminder_peel {id}`
- `reminder_list {}`

Receipts in `receipts.ts`. The persona prompt gains one line: a quick thing
to keep in mind is a post-it, not a task.

## 4. Idle nudges

### Presence

- `users.last_active_at` is updated at most once a minute by any
  authenticated API call, WebSocket ping, chat message, Telegram message, or
  Matrix message.
- The web client pings `POST /api/presence` every 60 s while the page is
  visible and the user has interacted in that minute.

### When a nudge may happen

The server checks each user every minute. A user is idle when all of these
hold:

- no work session is running,
- `now - last_active_at >= idle_nudge_min` (per-user setting, default 20,
  `0` turns it off),
- `now` is outside quiet windows and before the close-of-day time,
- the user has open, unpinned post-its,
- no idle trigger is already pending.

### What Note does

- On becoming idle, the server lays an idle trigger (a `trigger` event with
  `origin = 'idle'`) for now. It counts against the daily trigger budget, and
  none is laid when the budget is spent.
- The trigger session runs with the Trigger kind and an idle note in its
  context: the open post-its with ages and last nudges, and the minutes
  idle.
- It uses the existing tools:
  - `say` sends a notification through the ladder.
  - `stay_quiet` does nothing.
  - `wait_for` and `trigger_set` with `cancel_if: Replied` wait for an
    answer and lay a follow-up.
- The prompt `trigger.md` gains an idle section: when to stay quiet, one
  nudge at a time, escalate only after silence, never repeat the same
  post-it within an hour.
- `last_nudged_at` is stamped on the post-its a nudge names.

### Stopping

- A user becoming active cancels every pending idle trigger. This is a new
  `cancel_if` reason, `Active`, checked against `last_active_at` when the
  trigger fires.
- Replying cancels through the existing `Replied`.

### Later

Ringing the phone becomes a choice here once voice calls ship.

## 5. Matrix messaging and Connections

### The channel

`server/src/channels/matrix.rs`, a channel beside Telegram:

- **Account:** the bot account `@note:<server>` from the voice-calls work,
  logged in as its own device with its own access token (`[channels.matrix]`
  `homeserver`, `token_file`).
- **Receiving:** a `/sync` loop, filtered to linked rooms and `m.room.message`
  text. Each message from the linked account runs the same Talk turn as
  Telegram, with `Via::Matrix`.
- **Sending:** check-ins and messages go to the linked room as `m.text` with
  an HTML body. Buttons become a numbered list the user answers by number.
- **Rooms:** unencrypted DMs, as for calls.

### One link

- A Matrix link belongs to Note, not to the voice service: one row per user
  holding `mxid` and `room_id`, used by messaging and by calls alike.
- The voice-calls `voice_links` table is renamed `matrix_links`.
- The DM is opened by Note's own Matrix channel, and a room already opened by
  the voice service is adopted.

### Connections

- Settings gains a Connections group: Telegram, Matrix, Calls.
- The Matrix row links an account (Matrix ID, then accept the invite) and
  shows the linked ID.
- The Calls row appears once Matrix is linked, holding `ring_for` and
  "Ring me".
- Telegram moves into the group unchanged.

## 6. Translation plumbing

- **A lookup function:** `web/src/i18n/index.ts` exports `t(key, vars?)`.
  English strings live in `web/src/i18n/en.ts` as a flat object whose keys
  are grouped by view (`tasks.add`, `now.stop`).
- **Placeholders:** `{name}`.
- **Plurals:** `Intl.PluralRules` through `{count, one {…} other {…}}` in the
  few strings that need it.
- **Formatting:** `web/src/i18n/format.ts` holds the only date, time,
  relative-time and number formatters, each taking the active locale.
- **Locale:** today the locale is `en`, always. The type of the dictionary
  is derived from `en.ts`, so a second language file is checked for missing
  keys at compile time.
- **Scope:** every user-visible string in `web/src` moves into `en.ts`,
  including labels, placeholders, `aria-label`s, toasts and receipts. Direct
  `toLocale*` and `Intl.*` display calls move behind `format.ts`. Time-zone
  arithmetic in `sky.ts` and `zone.ts` stays.
- **Test:** a vitest check that fails on any JSX text node or string
  attribute matching `[A-Za-z]{3,}` outside `i18n/`, with an allowlist for
  class names and CSS.
- **Server:** notification titles and bodies stay English. They move into
  one module, `server/src/text.rs`, so a later language setting has one
  place to look.

## Testing

- **Server:**
  - route tests for inbox, refresh, reminders and presence;
  - the candidate route against a fixture day;
  - idle detection over a clock-controlled check (idle, active, budget,
    quiet window, cancel on activity);
  - Matrix channel tests against the mock homeserver pattern from voice
    calls.
- **Web:**
  - vitest for `t()` placeholders and plurals, and the formatters;
  - the hardcoded-string check;
  - pure helpers for board layout and tilt;
  - screenshots through the UI audit harness for the board, the idle Now
    face, the inbox and Connections, at phone and desktop widths.

## Risks

- **Moving every string in one pass is wide.** It lands last and alone, on
  a branch with nothing else in flight, and the hardcoded-string check keeps
  it complete.
- **The inbox refresh needs a host change** (the path unit). Without it the
  button writes a file nobody watches. So the button is hidden until
  `[inbox] refresh_signal` is set, and the host change ships in the same
  deploy.
- **Two Matrix processes on one account.** The server and the voice service
  each hold their own device and sync token, and each ignores the other's
  event types.
