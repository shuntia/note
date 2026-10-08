# Planning

## Tasks

A task is a title, a description, notes, a state (`open`, `in_progress`,
`done`, `dropped`), an optional duration in whole 5-minute blocks, an optional
place in Now, and up to one level of steps. Every task also carries:

- `due_at`: when the work is due, RFC 3339, stored in UTC, or `null`. A
  deadline, not a time to work; the plan is where work is scheduled. Steps
  never carry one (`422`).
- `external_id`: the id the creating system knows it by
  (`canvas:assignment:12345`), opaque, at most 200 bytes, unique per user.
- `actual_min`: minutes really worked, summed as sessions end, or `null`. A
  step's minutes count for its task too.
- `url`: a link back to the origin, `""` when none.
- `source`: `manual`, `agent` or `import`. A cookie writes `manual` unless the
  body names another; a bearer token writes `import` and never `manual`.
- `urgency`: `low`, `normal` (default) or `high`; a step reads its parent's.
- `pressing`: true when a live task is overdue or due within 48 hours.

`POST /api/tasks` takes `title` and any of `description`, `notes`,
`duration_min`, `parent_id`, `is_now`, `state`, `due_at`, `url`,
`external_id`, `source`, `notify`, `progress`, `category`, `goal_id` and
`urgency`; an unknown field is a `422`. `PATCH
/api/tasks/{id}` takes the same, with explicit `null` clearing `due_at`,
`duration_min`, `parent_id` and `external_id`. A bad `due_at` is `422`; an
`external_id` another task holds is `409`. Every listing returns `due_at`,
`external_id`, `url` and `source` on tasks and steps.

`DELETE /api/tasks/{id}` removes the task, its steps and their event links
(`204`, or `404` when not the caller's). A task with an `external_id` leaves a
tombstone ([importing.md](importing.md)). `POST /api/tasks/{id}/split` and
`POST /api/tasks/{id}/flatten` add and clear steps.

The queue (`GET /api/tasks/queue?limit=`, 1 to 20, default 5) ranks open
top-level tasks: Now first, then high, then pressing, then normal, then low,
nearest due date first within each, then oldest.

## Run order

Each day has a run order: up to 30 tasks or steps to work through, first to
last, stored per local date in `run_order` (schema v53, which also removed
unstarted automatic blocks). The nightly run sets the day it
plans; the day's `lay_day` wake-up ([agent.md](agent.md#wake-ups)) checks it
and sets it if the night left it empty or wrong; any session with the order
tools may change it. A finished, dropped or deleted item, or a step whose task
is, leaves the order on its own. The task of a running work session stays
first.

Now starts the order's first open item and falls back to the queue once the
order runs out: `GET /api/tasks/candidates?limit=` answers that list, one entry
per task. On the Tasks view, today's order heads the soon list and a long press
drags a row into or out of it.

- `GET /api/order` → `{date, task_ids}` for today.
- `PUT /api/order {task_ids}` replaces today's order and answers the same
  shape; an unknown, closed or repeated id, or more than 30, is `422`. It logs
  `order_changed`.

Deadlines in the agent's context: each Now and Later line ends with `due
today`, `due tomorrow`, `overdue` or `due <local date>`; the Later list leads
with the nearest deadline; a `Due soon: n in the next 3 days, m overdue` line
follows whenever there is anything to count. A briefing session gets the
deadline as a `Due: <local date and time>` line.

## The daily plan

The nightly run generates each day from the user's template
(`config/defaults/templates/default.toml`, or the user's own). Events:

- `fixed` events cannot move.
- `slide` events can slide within ±`slide_window_min` of their template time
  (`0` = unbounded).
- `drop` events can also be dropped by the agent.

Snoozing is separate: any undecided event can be snoozed ("not now"), which
re-fires it later and is not bounded by the slide window. Silent routines
(`alert = 0`) and blocks never deliver.

Event routes beside the plan reads (`GET /api/plan/today`,
`GET /api/plan/range`, `GET /api/day/{date}`):

- `POST /api/events/{id}/alert {alert}`: whether that event pings when it
  fires; a block is `409`.
- `POST /api/events/{id}/move_tomorrow`: drops it from today and plans it into
  tomorrow, returning `{"event_id", "date"}`; an already decided event is `409`.
- `POST /api/events/{id}/{shift,snooze,done,drop}`.

`GET /api/plan/today?calendar=1` answers `{"events": […], "calendar": […]}`
instead of the bare event array, so a client draws the day and its
commitments from one call. `POST /api/plan/{date}/carry` moves what is left of
a day to the next. Tasks are never laid into the day on their own: blocks come
from `plan_tasks` or the user.

## Calendar

Beside the plan, each user keeps a calendar of commitments: school, work, a
class, a commute. An entry is a title, a kind, a local `HH:MM` range, and
either a weekday set or a single date:

- `fixed`: a hard commitment. Nothing may be scheduled inside it.
- `busy`: a commute, a meal. Shows on the day and may be quiet, never blocks
  scheduling.
- `note`: informational (bin day, a birthday), never quiet.
- `free`: time the user has set aside for tasks, never quiet.

`quiet` defaults on and is forced off for a `note` or `free`. A recurring entry carries
`days` as a bitmask (Mon = 1 … Sun = 64) and optional `from_date`/`until_date`;
a one-off entry carries `on_date`. Any occurrence can be skipped by date
without touching the entry. Times are in the user's timezone; a calendar holds
at most 100 entries.

### Quiet windows

While the local time is inside a quiet occurrence, deliveries wait. An event
coming due there has its wall time moved to the end of the window, writes one
`delivery_deferred` row (`event <id> held until HH:MM by calendar <title>`),
and fires then by the normal path. Overlapping and adjacent quiet occurrences
merge, so back-to-back commitments defer once.

The calendar also shapes planning:

- A routine that would start inside a `fixed` occurrence moves to its end if it
  can slide, is left out if it can be dropped, and stays if it is `fixed`.
  Either adjustment writes `plan_adjusted`. A moved routine's `orig_wall_time`
  is where the day put it, so its slide window measures from there.
- `schedule_slide`, `schedule_reshape` and `schedule_insert` refuse a target
  inside a `fixed` occurrence: `10:00 is inside school 08:15-15:30`.
- The session context's `# Now` gains `Quiet until HH:MM (<title>)` during a
  window, and `# Today's plan` lists the day's occurrences under `Calendar:`
  (at most 12 lines).

### Calendar API

Cookie-authenticated, scoped to the caller, `{"error"}` on every failure:
`422` for a rejected or unknown field, `409` at the 100-entry cap or a taken
`external_id`, `404` for another user's entry, `400` for malformed JSON.

| Route | Body | Effect |
|---|---|---|
| `GET /api/calendar` | — | `{"entries": […]}`, each with its `exceptions` |
| `POST /api/calendar` | `{title, kind, quiet?, start_time, end_time, days?, on_date?, from_date?, until_date?, external_id?}` | `201` with the row |
| `PATCH /api/calendar/{id}` | any subset | the updated row |
| `DELETE /api/calendar/{id}` | — | `204` |
| `POST /api/calendar/{id}/skip` | `{date}` | `204`; that occurrence stops happening |
| `DELETE /api/calendar/{id}/skip/{date}` | — | `204`; it happens again |
| `GET /api/calendar/day/{date}` | — | `{date, occurrences: [{entry_id, title, kind, quiet, start, end}], quiet_now}` |

A row is `{id, title, kind, quiet, start_time, end_time, days, day_names,
on_date, from_date, until_date, external_id, created_at, updated_at,
exceptions}`; `external_id` is `null` for entries made here. On a `PATCH`,
`days` above zero makes an entry recurring and clears `on_date`, a non-blank
`on_date` makes it one-off and clears `days`, and an empty string clears
`from_date` or `until_date`. `quiet_now` is the `HH:MM` the current quiet window
ends, `null` for any date but today.

See [agent.md](agent.md#tools) for which sessions read and write the calendar.
Importers use the by-external routes
([importing.md](importing.md)).
