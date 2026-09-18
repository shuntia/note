# Sessions on the server, pomodoro, per-task notify, and the session face

Date: 2026-09-18. Status: approved (user: "minimize client state. go").

## Goal

The work session becomes a server object with the clock, the pause state, the
step position and the phase on it; the client paints what the server says.
Pomodoro mode alternates work and break rounds with notifications the server
sends. Each task says how its block announces itself (`none`, `chat`,
`notify`). Note checks in when a session reaches its planned end and again
the moment it ends. The session face matches the Horizon mocks
(`docs/superpowers/mockups/lamplight/Session*.dc.html`).

## Data model (one migration, v28)

```sql
ALTER TABLE tasks ADD COLUMN notify TEXT NOT NULL DEFAULT 'notify'
    CHECK (notify IN ('none','chat','notify'));

ALTER TABLE work_sessions ADD COLUMN mode TEXT NOT NULL DEFAULT 'single'
    CHECK (mode IN ('single','pomodoro'));
ALTER TABLE work_sessions ADD COLUMN work_min INTEGER;
ALTER TABLE work_sessions ADD COLUMN break_min INTEGER;
ALTER TABLE work_sessions ADD COLUMN phase TEXT NOT NULL DEFAULT 'work'
    CHECK (phase IN ('work','break'));
ALTER TABLE work_sessions ADD COLUMN phase_started_at TEXT;
ALTER TABLE work_sessions ADD COLUMN phase_paused_ms INTEGER NOT NULL DEFAULT 0;
ALTER TABLE work_sessions ADD COLUMN round INTEGER NOT NULL DEFAULT 1;
ALTER TABLE work_sessions ADD COLUMN paused_at TEXT;
ALTER TABLE work_sessions ADD COLUMN paused_ms INTEGER NOT NULL DEFAULT 0;
ALTER TABLE work_sessions ADD COLUMN step_index INTEGER;
ALTER TABLE work_sessions ADD COLUMN step_count INTEGER;
ALTER TABLE work_sessions ADD COLUMN step_name TEXT;
ALTER TABLE work_sessions ADD COLUMN notes TEXT NOT NULL DEFAULT '';
ALTER TABLE work_sessions ADD COLUMN conversation_id INTEGER REFERENCES conversations(id);
UPDATE work_sessions SET phase_started_at = started_at WHERE phase_started_at IS NULL;
```

## Settings

`user.toml`: `pomodoro_enabled: Option<bool>` (default false),
`pomodoro_work_min: Option<u32>` (default 25, 5..=120),
`pomodoro_break_min: Option<u32>` (default 5, 1..=60). Settings API body and
patch carry all three; Settings gets a "Pomodoro" fold row under HOME with a
switch and the two numbers. Constants `DEFAULT_POMODORO_WORK_MIN`,
`DEFAULT_POMODORO_BREAK_MIN` in config.rs.

## The session object

`GET /api/sessions/open` and every session route return this shape (`null`
from `open` when none):

```json
{
  "id": 9, "task_id": 42, "event_id": 2, "title": "Common App essay draft",
  "planned_min": 60, "started_at": "RFC3339", "paused_at": null, "paused_ms": 0,
  "mode": "pomodoro", "work_min": 25, "break_min": 5,
  "phase": "work", "phase_started_at": "RFC3339", "phase_paused_ms": 0, "round": 1,
  "step_index": 2, "step_count": 3, "step_name": "photos of the ceiling",
  "notes": "", "conversation_id": 31
}
```

Clock rules (server and client compute the same way):
`elapsed_ms = now - started_at - paused_ms - (paused_at ? now - paused_at : 0)`,
`phase_elapsed_ms = now - phase_started_at - phase_paused_ms - (paused_at ? now - paused_at : 0)`.
Resume adds the pause length to both `paused_ms` and `phase_paused_ms`. A
phase flip sets `phase_started_at = now`, `phase_paused_ms = 0`.

Routes (CurrentUser; 404 when the id is not the user's open session):

- `POST /api/sessions { task_id?, event_id?, title, planned_min?, step_index?, step_count?, step_name?, notes? }`
  → session. Ends any open session first (outcome `stopped`, with its
  farewell trigger). `mode`, `work_min`, `break_min` come from the user's
  pomodoro settings. Creates the session thread: a conversation titled
  `Session: {title}` (`via = 'web'`), stored as `conversation_id`.
- `POST /api/sessions/{id}/pause`, `POST /api/sessions/{id}/resume` → session;
  pausing twice or resuming an unpaused session is a no-op 200.
- `POST /api/sessions/{id}/step { step_index, step_name }` → session.
- `POST /api/sessions/{id}/skip_break` → session; only in `break`, flips to
  the next work round now.
- `POST /api/sessions/{id}/end { outcome: done | stopped }` (exists) → `{ ended }`.

## Pomodoro phases — runner

`runner::sweep_once` gains `work::tick(state, now)` under the lock: for each
open, unpaused pomodoro session whose `phase_elapsed_ms >= phase length`:

- `work → break`: flip; message title "Break" body "{break_min} min. Round
  {round} of {title} done." (`Urgency::Normal`, `conversation_id` = the
  session thread), delivered through the ladder off the lock; log
  `session_break`.
- `break → work`: flip, `round += 1`; message title "Round {round}" body
  "Back to {title}." Log `session_round`.

Both broadcast `changed` to the user's sockets so the face refetches. A
paused session never flips. `MAX_ROUNDS = 16` ends the session (`stopped`)
with the farewell trigger.

## Triggers around a session

Laid by `work::start` / `work::end`, all tied to the session
(`work_session_id`, so they never count against the budget and die with it):

- **Midpoint check** (single mode only, as today): `+min(planned/2, 25)`.
- **Planned end** (single mode, when `planned_min` is set): at
  `+planned_min`, prompt "The session on {title} has run its planned {n}
  minutes: ask in one line whether it is done or they want to keep going."
- **Farewell** (both modes): `work::end` first drops the session's pending
  triggers, then lays one at `+0` with prompt "The session on {title} just
  ended ({outcome}, {elapsed} min, round {round}): ask how it went in one
  line, name what is left, and offer the next step." Session triggers are
  exempt from `MIN_LEAD_MIN`, so this fires on the next sweep. Its
  `conversation_id` is the session thread, so the break reports below are in
  its history.

## Per-task notify and block starts

`tasks.notify` rides through `NewTask`, `TaskPatch`, the task JSON, and the
agent tools (`task_create`/`task_update` gain `notify`; the description says
what each value does). `runner::fire_due` also selects blocks
(`end_wall_time IS NOT NULL`) with `status = 'pending'` whose `event_tasks`
task has `notify != 'none'`, at their `wall_time` (quiet windows defer them
exactly like routines). They are marked `fired` and:

- `notify` → `OutboundMessage { title: "Starting now", body: "{task} · until {end}" }`
  through the ladder, `event_id` set, log `block_started`.
- `chat` → an assistant line "Starting now: {task}, until {end}." appended to
  the day's check-in thread (`talk::checkin_thread`) and `broadcast_changed`;
  nothing leaves the app. Log `block_started`.

Blocks with no task (template blocks) stay silent.

## Web

**State.** `session.ts` keeps only a cache of the last session JSON for the
first paint (`note.nowSession`); `GET /api/sessions/open` is the truth on
load, on every `changed` frame, and on tab focus. `FocusSession` becomes the
server shape plus derived helpers (`elapsedSec`, `phaseRemainingSec`,
`isPaused`). Pause, resume, step, skip-break and end call the routes and
replace the session with the reply. Starting from Home posts the task's
steps as `step_index/step_count/step_name`; "Done with this step" patches
the step task done as today, then calls `/step`; the last step calls `/end
{done}` and patches the task done with the elapsed note.

**Face (per the mocks).** Counter 76px desktop / 64px phone, title 22px /
20px, `k of n` 12px faint; the whole ring breathes (track and arc). On
desktop the topbar and jot fade out the moment a session starts and return
when it ends (same fade as idle-hide). On the phone the first face is only
the arc and the chevron: no upcoming list, no "So far today". The pulled
state, top to bottom: the 230px arc with the counter, the round pause
button, "Done with this step" full width, the jot line. Pause shows a play
glyph and dims the arc to 50%; over time counts up in sun-ink.

**Pomodoro face.** Work phase: eyebrow `ROUND {n}` (12.5px tracked
sun-ink) above the counter, counter = work remaining. Break phase: eyebrow
`BREAK`, counter = break remaining, title "Round {n} done", and under the
arc the jot in flow with placeholder "How did that round go?" bound to the
session thread (`api.talk(text, conversation_id)`); its reply unfolds
under the box and opens Chat on click, as elsewhere. A "Back to it" haze
button calls `skip_break`. Phase flips arrive as `changed` frames.

**Task notify control.** Tasks row overflow and the Home block menu gain a
three-way "Announce: None / Chat / Notify" choice writing `notify`.

## Testing

Server: clock math (pause, resume, flip with pauses), `work::tick` flips and
messages, MAX_ROUNDS, skip_break, step route, start ends the previous
session with a farewell, farewell fires on the next sweep (MockLLM says),
planned-end trigger only in single mode, block starts per notify value
(ladder vs thread vs silent) in `tests/sessions_api.rs` and
`delivery_day.rs`; settings validation for the three pomodoro fields; tool
fuzz survives `notify`. Web: `pnpm build` and headless screenshots of the
session face at 1440×900 and 390×844 (first face, pulled, break) beside
the mock shots.
