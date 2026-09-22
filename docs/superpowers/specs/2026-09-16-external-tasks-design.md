# External tasks: idempotent import from other systems

Requested by `schoolwork-check` (`~/Projects/schoolwork-check`), a Go program
that pulls every assignment from Canvas and Google Classroom and wants to
mirror them into the user's tasks from a timer, unattended. Everything here is
additive: existing clients keep working unchanged, every new field is
optional, and nothing changes the meaning of an existing one.

## Why the current API is not enough

Today an importer can only `POST /api/tasks {title}` and then `PATCH` the
description. That has four holes that no client-side trick closes:

- **No identity.** There is no field to hold the importer's own id, so a
  re-run cannot find the task it made last time. The only workarounds are a
  sentinel line inside `notes` or a local id map, both of which break the
  moment the user drops the task (dropped tasks vanish from `GET /api/tasks`,
  so the importer recreates them every hour).
- **No due date.** A planner without a place for "due Friday 23:59" cannot
  hold homework. `plans`/`events` are wall-clock scheduling, not deadlines.
- **Description cannot be set on create.** Two round trips per task, and a
  crash between them leaves an empty task behind.
- **`source` is hardcoded to `"manual"`.** The design doc names calendar and
  reminder sources, but the plumbing to set the value never landed, so
  imported tasks are indistinguishable from typed ones. That also means the
  Now-cap 409 fires on imports and the agent cannot tell "the user chose this"
  from "a script mirrored this".

## Scope

- Optional `external_id` and `due_at` on tasks, with an upsert route keyed on
  `(user_id, external_id)`.
- `description` and `notes` accepted on create.
- `source` settable over a bearer token, restricted to a small allow-list.
- Dropped tasks stay findable by `external_id` so the importer respects the
  user's decision instead of undoing it.
- Not in scope: attachments as first-class objects, tags, or a course entity.
  Extracted attachment text goes in `notes` for now; see "Later" for what
  would make that better.

## Schema (migration v12)

```sql
ALTER TABLE tasks ADD COLUMN external_id TEXT;
ALTER TABLE tasks ADD COLUMN due_at TEXT;          -- RFC 3339 UTC, like updated_at
ALTER TABLE tasks ADD COLUMN url TEXT NOT NULL DEFAULT '';
CREATE UNIQUE INDEX idx_tasks_external
    ON tasks(user_id, external_id) WHERE external_id IS NOT NULL;
```

`external_id` is opaque to the server, at most 200 bytes, and namespaced by
the client (`canvas:assignment:12345`, `classroom:c9:w2`). `due_at` is stored
as the same string format `updated_at` already uses, so `jiff` parsing and
ordering come for free. `url` is a deep link back to the origin; it is a
column rather than a line in `description` so the UI can render it as a link.

## API changes

### Create accepts more fields

`NewTask` gains `description`, `notes`, `external_id`, `due_at`, `url`, and
`source`, all optional. Add `#[serde(deny_unknown_fields)]` so a typo is a 422
instead of a silent drop, matching `TaskPatch`.

`source` is validated against `manual | agent | import` when the principal is
a session, and forced to `import` when the principal is a token unless the
body says otherwise and the value is in the allow-list. A token can never
write `manual`.

### Patch accepts the same fields

`TaskPatch` gains `description` (already present), `due_at`, `url`, and
`external_id`, the last two using the existing `present` helper so explicit
`null` clears them. Changing `external_id` onto a value another task already
holds is 409 `{"error": "external_id already in use"}`.

### Upsert

```
PUT /api/tasks/by-external/{external_id}
```

Body is `NewTask` without `external_id`. If a task with that
`(user_id, external_id)` exists, apply the body as a patch and return
`200 Updated`; otherwise create and return `201 Task`.

The route takes `TaskPrincipal`, not `CurrentUser`, so an importer runs on a
bearer token from the api-tokens work rather than a browser cookie. Every
other route in this spec that an importer needs (`GET /api/tasks` with the new
filters, the by-external delete) does the same. Everything else stays
cookie-only by design.

Two rules make this safe to run from a timer:

- **Dropped wins.** If the existing task is `dropped`, the upsert updates the
  stored fields but leaves `state` as `dropped` and returns `200` with the
  task, so the caller can see it was declined. The user dropping a task is a
  decision; the importer must not undo it.
- **Done from the outside is one-way.** An upsert may move `open` or
  `in_progress` to `done` (the LMS says it was submitted), but never `done`
  back to `open`. If the LMS reopens something, the client sends a fresh
  `external_id` or the user handles it by hand.

### List filters

`GET /api/tasks` gains three optional query parameters:

- `source=import` filters by source.
- `include_dropped=true` includes dropped tasks, so the importer can learn
  which of its tasks were dropped without a local file.
- `due_before=<RFC 3339>` for "what is due this week".

Default behaviour with no parameters is unchanged.

### Delete and tombstones

`DELETE /api/tasks/{id}` (api-tokens worktree) is a hard delete: the row, its
steps, and its `event_tasks` links go away and nothing is left behind. That is
right for a task the user typed, but it breaks "dropped wins" for imported
ones: with no row to find, the next timer run recreates the task the user just
deleted.

So a delete on a task that has an `external_id` becomes a tombstone instead:

- `state` is set to `dropped`, `title`, `description`, `notes`, and `url` are
  cleared to `''`, `due_at` is nulled, steps and `event_tasks` links are
  removed exactly as the hard delete does today.
- The row keeps `user_id`, `external_id`, `source`, and `updated_at`.
- It is hidden from `GET /api/tasks` like any dropped task and visible with
  `include_dropped=true`, so the importer can still learn it was declined.
- A later upsert against the same `external_id` follows the dropped-wins
  rule: fields refresh, state stays `dropped`.

Tasks without an `external_id` keep the hard delete unchanged. The response
is 204 in both cases, so callers need not know which path ran.

Add `DELETE /api/tasks/by-external/{external_id}` for symmetry, 204 or 404,
with the same tombstone behaviour.

If tombstones are unwanted, the alternative is to declare delete out of scope
for external tasks (the UI offers drop, not delete, on `source = import`) and
leave the hard delete as is. Either choice is fine for schoolwork-check; what
it cannot live with is a delete that silently invites recreation.

## Semantics worth writing down

- `duration_min`, `parent_id`, and `is_now` are untouched by upsert unless
  present in the body. An importer never sends `is_now`, so imports cannot hit
  the Now cap.
- `check_now_room` treats `source = import` like `Actor::Agent` if the field
  is ever set: trim, do not 409. Scripts cannot show a dialog.
- Title validation moves from the agent tool path into `tasks::create` and
  `tasks::update` so the HTTP path also rejects empty or oversized titles
  (1..=500 bytes). Currently a bulk importer can write an empty title straight
  into the DB.
- `description` and `notes` get the same 16 KiB cap the agent path already
  enforces, returning 422 rather than truncating, so the client knows to cut.

## Response shape

`Task` gains `external_id`, `due_at`, `url` (all nullable or empty-string),
and `source`. `TaskNode`, `Updated`, and the web `Task` type follow.

## Later, if the planner wants it

- **Attachments table** `task_attachments(task_id, name, url, mime, text)`
  with `GET /api/tasks/{id}/attachments`. Until then, schoolwork-check puts
  extracted text under a heading in `notes`.
- **A `group` or `course` string** on tasks for the UI to cluster by. Title
  prefixes work today but are ugly.
- **Webhook or `since=` cursor on list** so the importer can stop polling.

## What schoolwork-check will do in the meantime

Against the current API it creates with `POST`, patches description and notes,
embeds `schoolwork-check-id: <id>` as the last line of `notes`, and keeps a
small local map of external id to task id so dropped tasks are not recreated.
The day `PUT /api/tasks/by-external/{external_id}` lands, that whole layer is
deleted.
