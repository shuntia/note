# Goals, categories, task ordering, and polish — 2026-09-22

Follows the audit spec of the same day (`2026-09-22-ui-audit-and-redesign-design.md`),
whose D0 visual language applies throughout.

## G1. Categories

- `tasks.category TEXT NOT NULL DEFAULT ''` (migration). Free text, one per task; steps
  inherit their parent's on read and never carry their own.
- The importer (`import.md` sessions / `task_brief`) sets it to the course or source name;
  for existing rows a one-off backfill takes the text before the first ` — ` in the title
  when the title has one and the category is empty.
- `task_create`, `task_update`, `task_bulk_update` take `category`; `task_list` filters
  by it; `task_read` returns it. The Tasks API (`GET/POST/PATCH /api/tasks`) carries it.
- UI (Tasks): a row of category pills under the search box (`.seg`-style, wrapping),
  *All* first, then the user's categories by count. Selecting one filters both groups.
  A task row shows its category as `.meta` after the title only when the filter is *All*.
  The row menu gains *Category ›* (radio children: the user's categories, then *New…*
  which prompts inline).

## G2. Goals

- Table `goals (id, user_id, title, description, due_at, state IN
  ('open','done','dropped'), created_at, updated_at)`; `tasks.goal_id INTEGER NULL
  REFERENCES goals(id)` (top-level tasks only; the same rule as `due_at`).
- Tools: `goal_create`, `goal_update`, `goal_list` (with each goal's task counts and the
  next due task), and `goal_id` on `task_create`/`task_update`/`task_bulk_update`.
  Registry: GOALS domain in TALK, CHECKIN and NIGHTLY.
- Prompt guidance (`planning.md`, `persona.md`): when the user names something that
  takes weeks (an application, an exam, a project), make a goal, break it into 3–12
  tasks each with a due date spread back from the goal's, sizes in whole 5-minute
  blocks, and lay the near ones onto the plan with `plan_tasks`; on later check-ins
  review the goal's remaining tasks against its date.
- API: `GET /api/goals` (with counts), `POST`, `PATCH /api/goals/{id}`,
  `DELETE`. Tasks rows carry `goal_id` and `goal_title`.
- UI (Tasks): a *Goals* section above *Now* when any goal is open. Each goal is a
  row: title, `.meta` "3 of 9 tasks done, due Oct 25", a thin progress bar, a fold
  chevron that reveals the goal's open tasks (same task rows). Row menu: *Rename*,
  *Set due date*, *Mark done*, *Drop* (danger). Adding a goal: the *Add a task* box
  gains a menu (`⋯`) with *New goal…* which turns the box into a goal input (title +
  due date). A task's row menu gains *Goal ›* (radio children).

## G3. Ordering and search

- Server: each top-level task row gains `scheduled_at: string | null` — the local
  date-time of its next pending/snoozed plan block (from `events`/`event_tasks`),
  today or later.
- Default order in *Later*: by `scheduled_at` (soonest first, unscheduled last), then
  `due_at` (soonest first, undated last), then `created_at` newest first. A sort
  control at the group head (`.seg`: *Schedule*, *Due*, *Newest*, *Category*) with the
  choice persisted in `localStorage` (`note.taskSort`). *Category* groups rows under
  category sub-headings.
- Search: the *Add a task* box doubles as search: typing filters live over title,
  description, notes, category and goal title (case-insensitive, every word must
  match); Enter with a non-matching text still adds it as a task, with a small
  *Add "…"* hint when there are no matches. Clearing the box restores the list.
- A task row with a `scheduled_at` today shows `.meta` "18:15 today"; tomorrow →
  "Tue 18:15"; further → "Oct 3".

## G4. Password

- `POST /api/password { current, new }` → 204; 401 on a wrong current (rate-limited
  through the security limiter); 422 on a new password shorter than 8 chars. Every
  other session for the user is revoked except the caller's.
- Settings, *You* group: *Password ›* fold with current, new, confirm; the fold body's
  Status reads "Changed" or the error.

## G5. Web polish

- **Home scroll clamp.** One scroll gesture moves one stage and stops. Between the
  face and the Today stage the wheel/touch scroll is intercepted while the stage is
  pinned: a wheel event (or touch-move past 24px) starts `scrollToY(nextSnap)` and
  further wheel events are ignored until the scroll has settled and 500 ms have passed.
  Past the stage the page scrolls normally, but leaving the stage upward snaps back to
  its end first. Keyboard arrows and the chevron keep working. Reduced motion: the
  stops still hold, just without easing.
- **Row removal flicker.** Removing or completing a task must not make siblings
  jump: measure before the DOM change (`useLayoutEffect` FLIP already exists), keep the
  leaving row in the layout for the whole collapse, and give the list `contain: layout`
  so the bars' width does not re-flow. Verify with a slow-motion capture.
- **Context menu.** Right-click on a task row, a Today row, a calendar block, a thread
  row or a memory row opens that row's overflow menu at the pointer (`Overflow` gains a
  `ref` with `openAt(x, y)`; long-press on touch does the same). The `⋯` stays.
- **Layout audit.** At 390, 768, 1024, 1280, 1440 and 1920 wide: even padding at the
  rail on every view, no floating panel that is not anchored to something (the calendar
  sheet on desktop anchors to the right edge of the grid; the block menu to the block),
  no empty right half on Memory/Chat when a pane could use it, tables/lists never
  narrower than 60 % of the column at ≥1024. Fix what is found; record what was found
  in this file's appendix.
