# Learning at night, auto-breakdown, closing the day, Telegram buttons, the week

Date: 2026-09-18. Status: approved (user: "go. I'll be gone so deploy too").

Six pieces, split across four implementers. Migration steps are appended at
the END of `db::MIGRATIONS` (head is v30); the merge coordinator orders
steps from different branches, so never renumber.

## 1. Feedback learned at night (`server/src/learn.rs`)

Every ended session already records what the plan said and what happened.
The nightly turns that into two things:

- `tasks.actual_min INTEGER` (migration): `work::end` adds the session's
  elapsed minutes to its task's `actual_min` (and to the parent's when the
  session ran a step). Task JSON carries `actual_min`.
- `learning (user_id, key, value REAL, sample INTEGER, computed_at)` table.
  `learn::run_for_user` (called from `nightly::run_for_user` right after the
  harvest) computes `plan_factor` = median of `elapsed / planned` over the
  user's sessions with a `planned_min`, ended `done`, in the last 14 days,
  ignoring runs under 5 minutes; needs `sample >= 3`, else the row is absent.
  Clamped to 0.5..=2.0.
- The allocator multiplies every candidate's duration by `plan_factor`
  (rounded up to 5 minutes) when a factor exists. `plan_tasks` does the same.
- The context block's Settings line gains "Plan factor: 1.4× from 9
  sessions" (or "no plan factor yet"); the planning prompt tells the model
  to read it when it sets durations.

## 2. Auto-breakdown at allocation (`allocate.rs`)

- A task with open steps (children not done/dropped) is laid step by step:
  one block per step, `event_tasks` → the step's id, duration = the step's
  `duration_min`, else the parent's duration divided evenly, else
  `DEFAULT_BLOCK_MIN`. `PlanEvent.task` carries the step with its parent's
  title as `title` and the step's own name as `step`
  (`TaskRef { id, title, state, step: Option<String> }`).
- A task without steps whose duration exceeds `MAX_BLOCK_MIN = 90` or the
  largest free slot is chunked: blocks of at most 90 minutes, never under 25,
  laid across the day's free slots in order, all linked to the same task.
  Labels read "essay draft (1/3)". `planned_on` stays a set of tasks; a task
  may hold several auto blocks on one day.
- Both count toward `MAX_AUTO_BLOCKS`.

## 3. Close-the-day ritual

- `user.toml close_day_time: Option<String>` (HH:MM, default "21:30", blank
  = off); settings API + a Settings field under HOME.
- The nightly lays a **system trigger** at that time: `triggers::lay` gains
  `system: bool`; a system trigger is `origin = 'template'`, exempt from the
  budget and from `MIN_LEAD_MIN`, dropped and re-laid by `plan::generate`
  for that date if it already exists. Prompt: "It is the close of the day.
  In one or two lines say what is still pending and what got done, then ask
  whether to carry the rest to tomorrow. If they say yes, call plan_carry."
- Tool `plan_carry { date? }` (TALK, CHECKIN, TRIGGER): moves every pending
  task block of that date to tomorrow with `plan::move_to_tomorrow`, drops
  pending triggers of the day, returns `{ moved: n }`. Route
  `POST /api/plan/{date}/carry` (CurrentUser) does the same.
- Web: after `close_day_time` on today, the Today stage shows a "Close the
  day" card above the upcoming list: "n blocks still open" with "Carry to
  tomorrow" (calls the route, then refreshes) and "Leave them". Hidden when
  nothing is pending or once carried.

## 4. Telegram inline buttons

- `OutboundMessage.actions: Vec<Action>` with `Action { label, data }`
  (`data` ≤ 64 bytes, the Telegram limit). Producers: check-ins and
  triggers with an `event_id` → `Done` (`ev:done:<id>`), `Snooze 15`
  (`ev:snooze:<id>:15`), `Drop` (`ev:drop:<id>`); block starts with
  `notify` → `Start session` (`block:start:<event_id>`); the close-the-day
  message → `Carry to tomorrow` (`carry:<date>`). Other channels ignore
  `actions`.
- `TelegramChannel::send_message` sends `reply_markup.inline_keyboard` (one
  row) when actions exist. `getUpdates` asks for `["message",
  "callback_query"]`. A callback from a linked chat is checked against the
  user (the event or session must be theirs), applied (`plan::set_status`,
  `plan::snooze`, `work::start` from the block's task with its duration,
  `plan_carry`), answered with `answerCallbackQuery` (short toast text), and
  the message's keyboard is replaced by a line "✓ Done" /
  "✓ Snoozed 15 min" / "✓ Dropped" / "✓ Session started" / "✓ Carried"
  via `editMessageReplyMarkup` + `editMessageText` appended. Every applied
  callback logs `telegram_action` and broadcasts `changed`. Unknown or
  foreign data answers "That one is gone." and removes the keyboard.

## 5. The weekly debrief (`server/src/review.rs`)

- `reviews (user_id, week_start TEXT, content, created_at, PRIMARY KEY
  (user_id, week_start))` (migration). `week_start` is the Monday.
- On the nightly run whose plan date is a Monday, after the harvest and
  before planning: `review::run_for_user` builds a digest of the week just
  ended: tasks completed and dropped (titles, `actual_min`), sessions
  (title, planned, elapsed, outcome, overrun asks, force ends), triggers
  said/quiet, blocks carried, harvest fact counts, every conversation
  summary of the week, and the week's episodic memories (`memory::list`
  filtered by category and created date). Cap 32 KiB.
- `SessionKind::Review` (multi-turn, `REVIEW_MAX_TURNS = 8`, prompt
  `review.md` + context block, registry `memory_query`, `memory_read`,
  `memory_write`, terminal `review_write { text }` 1..=4000 bytes). The
  text is stored in `reviews`, delivered Monday morning with the debrief as
  a second message titled "Your week" (`channels::render` for a new event
  kind `review` laid by the nightly at the debrief's time + 1 minute), and
  the session also writes one episodic memory "Week of <date>: …".
- `GET /api/review?week=YYYY-MM-DD` (default: latest) → `{ week_start,
  content }` or 404. Web: a "Your week" fold beside the debrief fold, shown
  when a review exists for the current or previous week.

## 6. Semantic and episodic memory

- `memory_write` gains `until: Option<String>` (YYYY-MM-DD), like
  `inbox_decide`.
- `harvest.md` is rewritten to produce both kinds every night: semantic
  facts as now (people, preferences, routines, decisions, how to pitch),
  plus exactly one **episodic** entry for the day when anything happened:
  summary "Day <date>: <one line>", body 5–12 lines of what happened (what
  got done, what slipped, mood, notable exchanges), `until` = 90 days out.
  Procedural entries when the user described how they like something done.
- `context::settings_section` lists the count by category: "Memory: 24
  facts (15 semantic, 7 episodic, 2 procedural)". `memory::live_count`
  gains a per-category variant.
- The persona's memory-pass rule names episodic memory explicitly as where
  "what happened lately" lives.

## Testing

Server: learn (median, clamp, sample floor, actual_min accumulation);
allocator steps and chunks (labels, cap, slot fit); close-day trigger laid
once per date, carry moves blocks and drops triggers; Telegram keyboard
wire shape, each callback kind applied and answered, foreign event refused;
review digest, Monday-only, `review_write` stored and delivered, episodic
row written; memory_write until; harvest prompt test names both kinds;
settings validation for close_day_time. Every suite green, web build clean.
