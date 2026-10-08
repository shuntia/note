# Importing

Scripts that mirror outside work into Note (school assignments, course
announcements, a work calendar) authenticate with an API token
([security.md](security.md#api-tokens)) and use the routes below.

## Tasks by external id

An importer that re-runs on a timer finds the task it made last time through
the path:

```sh
curl -X PUT http://localhost:3271/api/tasks/by-external/canvas:assignment:12345 \
  -H 'Authorization: Bearer note_…' -H 'Content-Type: application/json' \
  -d '{"title":"Biology ch.4","due_at":"2026-09-19T23:59:00+09:00",
       "url":"https://canvas.example/a/12","notes":"worksheet attached"}'
```

The body is a task without its `external_id`, which the path carries (a body
naming a different one is `422`). The reply is the task with its steps:

- `201`: no such task existed; this call made it, `source` `import`.
- `200`: it existed. The importer owns the title, notes, due date and link,
  which refresh; the description, duration and steps belong to whoever briefed
  the task and are left alone. The state belongs to the user: `dropped` stays
  dropped, `done` never reopens, and `open` or `in_progress` moves to `done`
  when the body says the work was handed in.
- `410`: the user deleted it. Body `{"error", "external_id", "deleted_at"}`;
  the task is not made again.

`DELETE /api/tasks/by-external/{external_id}` deletes it (`204`, or `404`).
Deleting a task that carries an `external_id`, by either route or the agent's
`task_delete`, buries the id in `task_tombstones`, which is what the `410`
reports.

## Calendar by external id

`PUT /api/calendar/by-external/{external_id}` takes the body `POST
/api/calendar` takes and answers `201` with the entry it made, or `200` when it
refreshed the entry that id names: title, kind, quiet flag, times, days or date
and validity bounds are rewritten; the id and skipped dates are kept.
`DELETE /api/calendar/by-external/{external_id}` is `204`, or `404`; nothing is
buried, so the next run makes the entry again. Both take a bearer token or the
session cookie; a malformed body is `400`, invalid fields `422`, every error
`{"error"}`.

```sh
curl -X PUT 'http://localhost:3271/api/calendar/by-external/gcal:c_8f3a:t:busy' \
  -H 'Authorization: Bearer note_…' -H 'Content-Type: application/json' \
  -d '{"external_id":"gcal:c_8f3a:t:busy","title":"Club Meeting","kind":"busy",
       "start_time":"11:00","end_time":"12:00","on_date":"2026-09-19"}'
```

## Briefing a task

An importer can hand a task to Note's agent instead of running a model itself:

```sh
curl -X POST http://localhost:3271/api/tasks/42/agent \
  -H 'Authorization: Bearer note_…' -H 'Content-Type: application/json' \
  -d '{"context":"Due Friday. Worksheet handed out in class."}'
```

`context` is optional (`{}` or an empty body means none), capped at 32 KiB.
The reply:

```json
{
  "task_id": 42,
  "outcome": "briefed",
  "steps": [{ "name": "task_brief", "args": "…", "result": "…", "is_error": false }],
  "task": { "id": 42, "description": "…", "children": [] }
}
```

`outcome` is `dropped` when the task ends dropped, `unchanged` when no call
succeeded, `briefed` otherwise. `task` has the shape of a `GET /api/tasks`
entry. Failures, each with `{"error"}`: `404` (unknown or not the caller's),
`409` (the id is a step), `422` (context over 32 KiB), `429` (a session already
running, or the daily ceiling), `502` (model unreachable), `500`; `502`/`500`
also log `task_agent_error`.

The session is fresh, keeps nothing in the talk conversations, and has one
tool, `task_brief`, aimed at that task:

```json
{ "task_id": 42, "homework": true,
  "description": "Read chapter 4 and answer the questions.\nHand in: worksheet in class.",
  "duration_min": 45,
  "steps": [{ "title": "read chapter 4", "duration_min": 25 },
            { "title": "answer the questions", "duration_min": 20 }] }
```

It answers `{"task_id", "outcome": "briefed"|"dropped", "steps_applied"}`
(steps written, `0`, or `"kept"` when the task already had steps).
`homework: false` drops the task and writes `"Not homework: <reason>"` as the
description; the reason is required, at most 100 characters, and the rest of
the call is ignored. The brief is one transaction.

A successful `task_brief` ends the session; a rejected one gets a single retry,
so a session costs at most two provider calls. It cannot move anything into
Now, can drop only a task still `open`, and cannot set any state but `dropped`.
A failed session restores the task, its steps and event links exactly, so the
same id can be retried.

Ownership: the agent owns `description`, `duration_min` (written with
`duration_source: "agent"`), the steps and the `dropped` state; the caller owns
`title` and `notes`. Re-briefing a changed task leaves existing steps alone
(`"steps_applied": "kept"`) and updates description and duration only, noting
in the description when the steps no longer fit. Call
`POST /api/tasks/{id}/flatten` first to have steps regenerated.

The prompt is `config/defaults/prompts/import.md`.

## Reading an inbox item

An item that is not a task (an announcement, course material) can be handed
over for the agent to decide what, if anything, to remember:

```sh
curl -X POST http://localhost:3271/api/agent/inbox \
  -H 'Authorization: Bearer note_…' -H 'Content-Type: application/json' \
  -d '{"source_id":"lms:post:77","kind":"announcement",
       "context":"Posted 2026-09-18. Quiz on chapter 4 next Friday."}'
```

`source_id` is the caller's stable id, 1–200 characters of `A-Za-z0-9:._-`;
`kind` is `announcement` or `material`; `context` is the text, at most 32 KiB,
possibly empty. The reply:

```json
{
  "source_id": "lms:post:77",
  "outcome": "remembered",
  "reason": "the quiz date and the late-work rule",
  "memory_ids": ["8f2…", "b41…"],
  "steps": [{ "name": "inbox_decide", "args": "…", "result": "…", "is_error": false }]
}
```

`outcome` is `remembered` (facts written, ids in `memory_ids`), `task` (the
item asks for work; the importer creates the task) or `nothing`. Failures:
`400` malformed JSON, `422` bad `source_id`/`kind` or oversized context, `429`,
`502` (model unreachable or no decision), `500`; `502`/`500` log
`agent_inbox_error`. Nothing is written on a non-2xx reply.

The session is fresh, capped at two rounds, and sees
`config/defaults/prompts/inbox.md` as its whole system prompt. Its tools are
`memory_query`, `memory_read` and `inbox_decide`:

```json
{ "source_id": "lms:post:77", "outcome": "remembered",
  "reason": "the quiz date and the late-work rule",
  "facts": [
    { "summary": "Biology quiz on chapter 4",
      "body": "Biology: the chapter 4 quiz is on 2026-09-25, per \"Quiz Friday\".",
      "until": "2026-09-25" },
    { "summary": "Biology late work policy",
      "body": "Biology: late work loses 10% a day, per \"Quiz Friday\"." }]}
```

It answers `{"outcome", "reason", "memory_ids", "superseded"}`. The
`source_id` must be the session's; `remembered` takes 1 to 10 facts, the other
outcomes none; `reason` is 1–200 characters, `summary` 1–120, `body` up to
2 KiB; `until` is `YYYY-MM-DD`. Facts are written as `semantic`, embedded and
indexed like `memory_write`.

Each fact gets a `memory_sources` row (`user_id`, `source_id`, `memory_id`).
Before writing, a decision archives every fact that source produced before and
reports the count in `superseded`, so re-sending an edited item replaces what
Note remembers, and a re-send deciding `nothing` or `task` clears it. The map
is per user.
