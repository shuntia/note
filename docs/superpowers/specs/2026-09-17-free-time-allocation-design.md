# Free time, nightly task allocation, and the unified day

Date: 2026-09-17. Status: approved for implementation (autonomous session).

## Goal

The user declares recurring or one-off **free time** on the calendar. Every
night the server fills that free time with open tasks on its own, and the
result shows up as task blocks in both the calendar and the today view. The
today view also shows what already happened today (events decided, tasks
completed, check-ins answered), not only what is ahead.

## Data model (migrations v20–v22, appended to `db::MIGRATIONS`)

### v20 — calendar kind `free`

`calendar_entries.kind` gains `'free'`. SQLite cannot alter a CHECK, so the
migration rebuilds both calendar tables: create `calendar_entries_v2` (same
columns, CHECK adds `'free'`) and `calendar_exceptions_v2` (FK to `_v2`), copy
rows, `DROP TABLE calendar_exceptions` **first** (so dropping the parent
cascades nothing), then `DROP TABLE calendar_entries`, rename `_v2` tables to
the original names (renaming the parent updates the child's FK), recreate
`idx_calendar_entries_user`. A db test inserts an entry + exception on v19,
migrates, and asserts both survive with the same ids.

Semantics of `free`: never quiet (forced 0 like `note`), never blocks
scheduling (`overlaps`/`conflict` stay fixed-only), and it is the **only** kind
the allocator places tasks into. `calendar::KINDS` and the tool descriptions in
`tools/mod.rs` describe it as "time the user has set aside for tasks".

### v21 — event provenance and decision time

```sql
ALTER TABLE events ADD COLUMN origin TEXT NOT NULL DEFAULT 'template'
    CHECK (origin IN ('template','agent','auto','user'));
ALTER TABLE events ADD COLUMN decided_at TEXT;
```

- `origin`: `plan::generate` → template; `schedule_insert`, `plan_tasks`,
  `notify_send` → agent; the allocator → auto; `POST /api/events/...` user
  edits leave it alone (origin is where the row came from, not who touched it).
- `decided_at` (RFC3339 UTC) is set by `plan::set_status` (done/dropped),
  `plan::snooze`, and `plan::move_to_tomorrow` on the original row. Reopening
  is not a thing today, so it is never cleared.

### v22 — task completion time

```sql
ALTER TABLE tasks ADD COLUMN completed_at TEXT;
```

Set to `now` whenever a write moves `state` to `done`, cleared when a write
moves it away from `done`. `tasks::done_between` switches to `completed_at`.
Backfill in the migration: `UPDATE tasks SET completed_at = updated_at WHERE
state = 'done'`.

## The allocator — `server/src/allocate.rs`

Pure planning core plus one DB wrapper.

```rust
pub struct Window { pub start: u16, pub end: u16 }            // minutes of day
pub struct Candidate { pub id: i64, pub minutes: u16, pub is_now: bool,
                       pub due: Option<jiff::civil::Date>, pub created: String }
pub struct Placement { pub task_id: i64, pub start: u16, pub end: u16 }

/// First-fit in priority order: is_now, then earliest due (overdue first,
/// no due date last), then oldest created. A task that fits nowhere is skipped
/// and the next one is tried. `GAP_MIN = 5` separates placements.
pub fn pack(free: &[Window], busy: &[Window], tasks: &[Candidate], cap: usize) -> Vec<Placement>
```

Free windows minus busy ranges (fixed calendar occurrences, every non-dropped
event on the plan using its span or block range) give the usable slots. A task
is placed at the earliest slot that holds its full duration
(`duration_min`, else `DEFAULT_BLOCK_MIN = 25`). `MAX_AUTO_BLOCKS = 8` per day.

```rust
pub struct Outcome { pub placed: Vec<Placement>, pub cleared: usize }
/// Removes this day's `origin = 'auto'` blocks that are still pending, then
/// packs the day's free time. When `date` is today, slots before `now + 10 min`
/// are treated as busy. Returns the placements, which are inserted as
/// task-backed blocks exactly the way `plan_ops::plan_tasks` does
/// (`alert = 0`, `flexibility = 'drop'`, `origin = 'auto'`, `event_tasks` row).
pub fn run(conn: &Connection, user_id: i64, tz: &TimeZone, date: Date, now: Timestamp) -> Result<Outcome>
```

Candidates are the user's open/in_progress top-level tasks that have no
non-dropped event on that date (reuse `plan_ops::already_planned`). Done and
fired auto blocks are never cleared. Logs one `plan_allocated` row:
`"{date}: {n} placed, {cleared} cleared"` (only when either is non-zero).

Callers:

1. `nightly::run_for_user` — right after `plan::generate`, before the LLM
   session. Failure logs `allocate_error` (throttled) and the run continues.
2. Agent tool `plan_auto { date? }` in the NIGHTLY and TALK registries — "lay
   open tasks into the day's free time; replaces earlier automatic blocks that
   have not started". Returns `{plan_date, placed: [{event_id, task_id, start,
   end}], cleared}`.
3. `POST /api/plan/{date}/allocate` (CurrentUser) → same JSON, 400 bad date,
   422 when the date is in the past. The today view's "Fill my free time"
   button calls it.

`planning.md` gains one line after step 1: automatic task blocks are already
laid into free time; drop the ones that do not belong and call `plan_auto`
again if free time changed.

## The unified day — `GET /api/day/{date}`

One read that every day-shaped surface uses. `PlanEvent` gains
`origin: String` and `task: Option<TaskRef { id, title, state }>` (joined
through `event_tasks`; `plan_list` keeps its `task_id`).

```json
{
  "date": "2026-09-17",
  "events": [PlanEvent],            // whole plan, every status, as plan/today
  "calendar": [Occurrence],         // all kinds, free included
  "free": [{"start":"16:00","end":"18:30"}],   // free occurrences minus fixed ones
  "quiet_now": "15:30" | null,      // today only
  "history": [HistoryRow]           // empty for dates other than today or the past
}
```

`HistoryRow { at: RFC3339, time: "HH:MM" local, kind, label, event_id?, task_id?,
conversation_id? }` with `kind ∈ event_done | event_dropped | event_moved |
event_snoozed | event_fired | task_done | checkin }`, sorted by `at`. Sources:
events with `decided_at`/`fired_at` on that date (an event whose status is
`fired` and whose end passed without a decision is **not** history; it stays
in the upcoming list as overdue), tasks with `completed_at` on that local
date, and the day's check-in conversation when it has a user row (label
"Check-in answered"). `event_moved` uses `moved_to` for its label.

`GET /api/plan/today` stays as it is (bare array / `?calendar=1` object) for
older clients; the web client stops using it. A plan for the requested date is
generated on read exactly as `plan_today` does today. `GET /api/plan/range?from=&to=`
(≤ 14 days) returns `{ "days": { "YYYY-MM-DD": [PlanEvent] } }` for dates that
already have a plan, so the week grid can draw task blocks without creating
plans. Both routes: 400 on a bad date/range.

## Web

- `api.day(date)`, `api.planRange(from, to)`, `api.allocate(date)`; new types
  `DayView`, `HistoryRow`, `TaskRef`, `origin` on `PlanEvent`, `'free'` in
  `CalendarKind`.
- `Home.tsx` loads `api.day(today)` once per refresh and owns the result: the
  spine, the upcoming list, the new "So far today" fold and the calendar
  section all read from it. `Today.tsx` is deleted; `DebriefFold` moves to
  `web/src/debrief.tsx`. `pulse.tsx` reads the same `DayView`.
- **So far today** (`web/src/sofar.tsx`): a fold under the upcoming list
  (same idiom as `DebriefFold`, storage key `note.sofarFolded`), rows
  `HH:MM · label` with a sage check for done, a struck title for dropped, an
  arrow for moved. Empty state: "Nothing yet". The DayLine draws past decided
  events as 6px ticks on the gone bar (`.dl-past.done` sage, `.dl-past.dropped`
  hollow).
- **Task blocks**: events with `task` render as `.dl-span.task` on the spine
  and `.band.task` in the calendar grid/day bands: `--haze-strong` fill with a
  2px `--sun-ink` left rule; `origin === 'auto'` gets a small "auto" eyebrow in
  the tooltip. The upcoming list row for a task block has Start (opens the
  focus session for that task), Done, and the overflow (Drop today, Move to
  tomorrow). Clicking a task block in the calendar opens the same row menu
  rather than the entry sheet.
- **Free time** in the calendar: `free` is a fourth kind in the entry sheet
  and `SheetMenu` (label "Free time"); band style `.band.free` = dashed
  `--sun-ink` outline, no fill, title in `--sun-ink`; `.dl-band.free` = 16%
  `--sun` mix. The week grid and day bands layer that week's task blocks over
  the free band (`api.planRange` for the visible week).
- A "Fill my free time" text button in the calendar section header (only when
  the day has a free occurrence) calls `api.allocate(date)`, then refreshes.

## Testing

Server: `allocate::pack` unit tests (priority order, gap, skip-when-no-fit,
cap, busy subtraction, today's cut-off); DB migration v20 preservation test;
`tests/day_api.rs` covering the shape, history rows for each kind, free
subtraction, range bounds, allocate route (422 past, 200 idempotent re-run
clears pending auto blocks but keeps done ones); `delivery_day.rs` extended so
the nightly run lays blocks into a free window. Web: `pnpm build` clean.
