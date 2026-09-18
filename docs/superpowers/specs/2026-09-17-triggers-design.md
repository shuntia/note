# Trigger points, waits, and work sessions

Date: 2026-09-17. Status: spec, awaiting go-ahead.

## Goal

Note reaches out on its own terms instead of firing canned check-ins. The
nightly plan lays a few **trigger points**, each a moment with a prompt.
When one fires, a short non-chat session reads the situation and either says
something or stays quiet. Sessions can also **wait**: schedule a follow-up
that fires unless the user has replied, finished the task, or decided the
event. During a **work session** progress checks are unlimited, because
accountability is the point; outside one they are budgeted, and Note asks
before exceeding the budget.

## Data model (one migration)

```sql
ALTER TABLE events ADD COLUMN prompt TEXT NOT NULL DEFAULT '';
ALTER TABLE events ADD COLUMN cancel_if TEXT
    CHECK (cancel_if IS NULL OR cancel_if IN ('replied','task_done','event_decided'));
ALTER TABLE events ADD COLUMN cancel_ref INTEGER;          -- task or event id for cancel_if
ALTER TABLE events ADD COLUMN conversation_id INTEGER REFERENCES conversations(id);
ALTER TABLE events ADD COLUMN work_session_id INTEGER REFERENCES work_sessions(id);
ALTER TABLE events ADD COLUMN created_at TEXT;             -- when the trigger was laid

CREATE TABLE work_sessions (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id),
    task_id INTEGER REFERENCES tasks(id),
    event_id INTEGER REFERENCES events(id),
    title TEXT NOT NULL,
    planned_min INTEGER,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    outcome TEXT CHECK (outcome IS NULL OR outcome IN ('done','stopped'))
);
CREATE INDEX idx_work_sessions_open ON work_sessions(user_id) WHERE ended_at IS NULL;

CREATE TABLE trigger_budgets (
    user_id INTEGER NOT NULL REFERENCES users(id),
    date TEXT NOT NULL,
    extra INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (user_id, date)
);
```

A trigger is an event with `kind = 'trigger'`, a routine (no end), `alert = 1`,
`flexibility = 'drop'`, `origin = 'agent'`, `channel` push or voice, its
`prompt`, and optionally a cancel rule, a thread, and a work session. It
shows on the plan like any event and can be dropped by the user.

## Budgets

- `triggers_per_day: Option<u32>` in `user.toml` (default 4; settings API +
  a Settings field "Check-ins from Note: up to N a day").
- Today's allowance = `triggers_per_day + trigger_budgets.extra`. Spent =
  agent-laid triggers for the plan date with `work_session_id IS NULL` and
  status not `dropped`.
- Work-session triggers never count and never refuse for budget.
- `trigger_budget { extra: 1..=10, reason }` (TALK and CHECKIN only) adds to
  today's `extra` and logs `trigger_budget`; the receipt reads "Raised
  today's check-in budget by N".

Prompt lines. Persona (Talk/Checkin): "You may lay up to {allowance} trigger
points a day on your own; {spent} are used. If the day needs more, ask the
user first, and call trigger_budget only after they agree." Planning
(Nightly): "Lay two to four trigger points for the day with trigger_set: after
the first task block, at the end of free time, before anything due. That is
the whole budget you lay alone; a check-in is where to ask for more." The
allowance and spent count come from the context block's Settings section.

## Tools

All three laying tools share one implementation and one refusal shape:
`cap_reached` names the way out ("ask the user; trigger_budget after they
agree"), `too_soon` (under 10 minutes ahead), `past` (today only, in the
future; the nightly lays for its plan date), `not_found` for a foreign ref.
Each returns `{ event_id, at, cancel_if }` and logs `trigger_laid`.

- `trigger_set { at: "HH:MM" | "+Nmin", prompt, channel?, thread?: conversation_id }`
- `wait_until { at | +Nmin, prompt, thread? }` → `cancel_if = 'replied'`:
  skipped if a user message lands in the thread after the trigger was laid.
- `wait_for { task_id | event_id, until: "HH:MM" | "+Nmin", prompt }` →
  `cancel_if = task_done` (task state done or dropped) or `event_decided`
  (status done or dropped).

Registries: NIGHTLY, TALK, CHECKIN, TRIGGER get all three; `trigger_budget`
is TALK and CHECKIN only. `plan_list` shows `prompt` and `cancel_if` on
trigger rows; `schedule_drop` works on them.

Trigger-session terminals: `say { text }` (1..=1200 bytes) and
`stay_quiet { reason }`.

## Firing — `SessionKind::Trigger`

The runner's `fire_due` treats a trigger like any routine (quiet windows
defer it). `deliver_event` branches on `kind == 'trigger'`:

1. Cancel check under the lock: rule met → `status = 'dropped'`,
   `decided_at = now`, log `trigger_cancelled`; nothing else happens.
2. Otherwise mark `fired`, then off the lock run a Trigger session
   (`TRIGGER_MAX_TURNS = 6`, `talk_gate` respected; busy → retry next sweep
   by leaving the event `pending`): system prompt `trigger.md` + persona +
   the context block; history = the thread's last 12 turns with the summary
   note; opening = the prompt plus a situation line: laid at, meant for, work
   session (title, started, planned, steps done since the last check, last
   user message time). Registry TRIGGER: memory_query/read, task_list/read/
   search, plan_list, the three laying tools, `say`, `stay_quiet`.
3. `say` → append the text as an assistant row to the thread
   (`conversation_id`, else the day's check-in thread via `talk::checkin_thread`),
   deliver `OutboundMessage { title: "Note", body, conversation_id }`
   through the ladder (voice when `channel = 'voice'`), log `trigger_said`.
   `stay_quiet` → `status = 'done'`, log `trigger_quiet` with the reason.
   Session error → log `trigger_error` (throttled), event stays `fired`,
   nothing is sent.

`trigger.md`: you are following up on your own plan; say one or two warm
sentences or nothing; stay quiet when the user messaged or marked a step done
in the last ten minutes, when what you would say is already on screen, or
when the prompt no longer applies; you may lay one follow-up with wait_until
or wait_for; never lay more than one per firing.

## Work sessions

- `POST /api/sessions { task_id?, event_id?, title, planned_min? }` → `{ id }`;
  ends any open session first. `POST /api/sessions/{id}/end { outcome }`
  → drops that session's pending triggers (`trigger_cancelled`) and stamps
  `ended_at`. `GET /api/sessions/open` for a reloading client.
- On start the server lays the first check itself: a work-session trigger at
  `+min(planned_min / 2, 25)` minutes (default 25) with the prompt "Progress
  check on {title}." A Trigger session inside a work session may lay the next
  check with `wait_until`, at least 10 minutes out and never past the planned
  end plus 15 minutes; these are uncapped.
- Client: `session.ts` gains `serverId`; Home's start/finish call the routes
  (`changeSession`), and a reload asks `/api/sessions/open`. Tab and
  session UI otherwise unchanged.

## Today view

Triggers render as `.dl-span.trigger` (a hollow 8px ring on the spine) with
label "Note checks in" and the prompt in the tooltip; the upcoming list shows
them with the existing Drop today action. Cancelled triggers do not appear
in history; said ones appear as the assistant message in the thread.

## Testing

Unit: budget arithmetic (allowance, spent, extras, work-session exemption);
each refusal; cancel rules against replied/task/event state; first-check
timing; session end drops its triggers. Integration `tests/triggers_api.rs`:
a MockLLM Trigger session that says (thread row + MockChannel delivery +
`conversation_id`), one that stays quiet, one that lays a follow-up, a cancel
at fire time, a work session laying and ending, `trigger_budget` from Talk;
`delivery_day.rs` extended with a nightly-laid trigger firing. Prompt test
extended for `trigger.md`; registry invariant test extended.
