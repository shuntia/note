# Agent

## Providers

With no `[providers.llm]` the server runs on a built-in mock provider: no API
keys, fully runnable offline; plans are still generated from templates and
nights end with the fallback debrief.

`[providers.llm]` takes `kind = "anthropic"` or `"openai"` (any
OpenAI-compatible endpoint: OpenRouter, NVIDIA NIM at
`https://integrate.api.nvidia.com/v1`), `base_url`, `model`, and the key from
`api_key_file` (bare key, surrounding whitespace ignored) or the env var named
in `api_key_env`; the file wins if both are set. Keys never live in config
files.

- `timeout_secs` (default 45) caps one chat call, keeping a session inside the
  100 seconds a Cloudflare tunnel allows a response. `background_timeout_secs`
  (default 180) is the same cap for sessions nobody waits on, such as the
  nightly letter.
- `reasoning`: `"none"` (default), `"low"`, `"medium"` or `"high"`. Anything
  but `"none"` sends `reasoning: {"effort": …}` and reads the text back from
  `message.reasoning` or `message.reasoning_content`. Reasoning is shown live
  and stored on the assistant row (capped at 32 KiB), never fed back into the
  next round.
- `cache_ttl_min`: how long the endpoint keeps a prompt cached; it caps how
  long a conversation may idle before the summary pass (`[agent]
  idle_summary_min`) replays it.

`[providers.embeddings]` (OpenAI-compatible, e.g. a local llama.cpp router)
makes memory search hybrid (lexical + vector); it falls back to lexical when
the provider is down.

## Sessions

Session kinds: talk (`POST /api/talk`), check-in, trigger (a check-in Note
starts on its own), nightly, harvest, review, summarize, task briefing and
inbox reading ([importing.md](importing.md)), share-link visitor
([share-links.md](share-links.md)), and voice calls ([voice.md](voice.md)).

`POST /api/talk {message, conversation_id?}` runs one turn. Omit
`conversation_id` and the server opens a conversation titled from the message;
pass one and its last 32 text turns are replayed first. The reply carries
`conversation_id`, the reply text, `steps` (every tool call with arguments,
result, error flag, and `thinking`, the reasoning of the round that made it, on
the round's first call), `reasoning` for the answering round, and `thought_ms`,
the wall clock from the first provider call to the reply. Progress streams over
the WebSocket ([delivery.md](delivery.md#websocket)).

Conversations: `GET /api/conversations`, `PATCH/DELETE /api/conversations/{id}`,
and `GET /api/conversations/{id}/messages` (user, assistant and tool rows in
order; each carries `reasoning`, and the assistant row `thought_ms`).

## Nightly run

At each user's `nightly_time` (default 03:00, their timezone) the server
generates the day's plan from their template, lets the agent adjust it and
write a morning debrief, and stores it. The last step is the brief it leaves
itself in `nightly_notes.md`. If the model is unreachable the plan still exists
and a fallback debrief says so. The nightly run also archives expired memory
facts.

## Prompts

Agent behaviour lives in `config/defaults/prompts/*.md` (`ja/` for Japanese),
overridable per user under `config/users/<user>/prompts/`. Changing tone or
policy is a file edit, not a deploy. Over HTTP:

- `GET /api/prompts/{name}` → `{name, content, custom}`, `content` being the
  effective text.
- `PUT /api/prompts/{name} {content}` writes the user's override.
- `DELETE /api/prompts/{name}` goes back to the default.

Any name without a prompt file is a `404`. `import` drives task briefing,
`inbox` item reading, `share` share-link sessions, `search` the web-search
summarizer, `planning` the nightly run, `voice` calls.

## Context

Every talk, check-in and nightly session's system prompt carries, in order:

1. The standing document, `config/users/<user>/standing.md`, edited in place by
   the agent through `context_edit`. Capped at 64 KiB; an edit that would cross
   it is rejected and the file left alone.
2. `# Notes from last night (written <date>, <age>)`: `nightly_notes.md`,
   written by the nightly agent for itself through `nightly_notes_write`
   (nightly only; 1 byte to 3 KiB of plain lines, no markdown headers, atomic).
   Its first line is a `<!-- written YYYY-MM-DD -->` marker in the user's
   timezone; past three days the header gains `(stale)` and the notes stay. A
   night that never calls the tool logs `nightly_notes_missing` and leaves
   yesterday's notes in place. What belongs in it is the closing step of
   `prompts/planning.md`.
3. A block rebuilt from the database on every call: the real-time line
   (weekday, local time, timezone and UTC offset, UTC instant, part of day);
   where the day stands against its first and last planned event and the
   nightly run; today's plan with current and next event marked and statuses
   counted; Now tasks with their steps and the Later list (at most ten titles,
   led by the nearest deadline), each line carrying its deadline; how much is
   due inside three days; what was finished today; the latest debrief in
   excerpt; whether tomorrow is planned; settings that shape advice and the
   live memory-fact count; the last ten user-meaningful `event_log` rows
   (deliveries, sessions, tokens, admin and security rows left out). Titles,
   states, durations and step titles only: descriptions and notes never reach
   the prompt. The block is held under 6 KiB by shortening the Later list, then
   the debrief, then the activity tail, and last night's notes last; the
   real-time line, Now tasks and plan are never trimmed.

Scoped sessions (task briefing, inbox, share) get neither the document nor the
block.

## Memory

Per-user long-term memory lives under
`data/memory/<user>/{semantic,episodic,procedural,archive}/`: one markdown fact
per file, frontmatter with a one-line summary. Facts are never deleted:
superseding writes a replacement and moves the old file to `archive/`. The
SQLite FTS index over the files is derived and rebuilt at startup; the files are
the source of truth to back up. Built-in facts about Note itself are seeded
from `config/defaults/memory/{en,ja}/`.

A fact may carry `until: YYYY-MM-DD` in its frontmatter. Every nightly run
archives live facts whose `until` is more than a day past and logs one
`memory_expired` row with the count.

Read-only routes, scoped to the caller:

- `GET /api/memory?category=&q=&limit=` → `{"items":[{"id","category","summary"}]}`,
  live facts only. Without `q`, newest first, optionally filtered to
  `semantic`, `episodic` or `procedural` (anything else is `400`). A non-blank
  `q` runs a lexical search and ignores `category`. `limit` defaults to 100,
  clamped to 1–200.
- `GET /api/memory/{id}` → `{"id","category","summary","body","supersedes","created","archived"}`;
  unknown, malformed or another user's id is `404`.

## Tools

Model-facing capabilities are typed tool calls dispatched through a registry
per session kind (`server/src/tools/mod.rs`). Every call is validated,
size-capped and transactional; failures return typed rejections and never
leave partial state.

| Tools | Offered to |
|---|---|
| `memory_query`, `memory_read` | check-in, talk, nightly, trigger, inbox, harvest, review, call |
| `memory_write` | check-in, talk, nightly, harvest, review, call |
| `context_edit` | talk, nightly |
| `task_list`, `task_search`, `task_read` | check-in, talk, nightly, trigger, share, call |
| `task_create`, `task_update`, `task_split`, `task_delete` | check-in, talk, nightly, call |
| `task_bulk_update` | talk, nightly |
| `goal_create`, `goal_update` | check-in, talk, nightly, call |
| `goal_list` | check-in, talk, nightly, share, call |
| `note_add`, `note_update`, `note_done`, `note_list` | check-in, talk, nightly, trigger, call |
| `plan_list` | check-in, talk, nightly, trigger, share, call |
| `plan_tasks`, `plan_auto` | talk, nightly |
| `plan_carry` | check-in, talk, trigger |
| `schedule_slide`, `schedule_snooze`, `schedule_drop`, `schedule_reshape` | check-in, talk, nightly, call |
| `schedule_insert`, `notify_send`, `nightly_notes_write` | nightly |
| `calendar_list` | check-in, talk, nightly, share, call |
| `calendar_add`, `calendar_update`, `calendar_remove`, `calendar_skip` | talk, nightly, call |
| `trigger_set`, `wait_until`, `wait_for` | check-in, talk, nightly, trigger |
| `trigger_budget` | check-in, talk |
| `web_search` | check-in, talk, nightly, call (with `[search]`) |
| `batch` | check-in, talk, nightly, trigger, harvest, review |
| `say`, `stay_quiet` | trigger |
| `cancel_job`, `hang_up` | call |
| `task_brief` / `inbox_decide` / `summary_write` / `harvest_done` / `review_write` / `share_note` | import / inbox / summarize / harvest / review / share, each ending its session on success |

Details:

- `task_list` filters by state, keyword, age, Now, urgency, `due_before` and
  `due_after` (`YYYY-MM-DD`, start of that local day) and `overdue`;
  `sort: "added"` (newest first, default), `"due"` (nearest deadline first,
  undated last) or by urgency (high, pressing, normal, low). `task_search`
  matches every word against titles, then the rest of the text. `task_read`
  shows one task as `GET /api/tasks` renders it, plus `url`, `external_id`,
  `source` and when it was added.
- `task_create`/`task_update` take `due_at` as RFC 3339 or bare `YYYY-MM-DD`
  (end of that local day); an empty string clears it.
- `task_bulk_update` sets a state, moves Now or deletes across up to 50 tasks,
  all or nothing. `task_delete` matches `DELETE /api/tasks/{id}`.
- `plan_tasks` lays up to 10 tasks out as consecutive silent blocks on a day's
  plan, each as long as its duration, refusing rather than reshuffling on
  overlap.
- Calendar tools take weekdays as names (`["mon", "tue"]`); `calendar_list
  {date?, days?}` reads 1 to 14 days as `{days: [{date, occurrences}]}`.

`batch` runs 1 to 10 of those calls in one model round. Each sub-call takes the
path and transaction it would take alone and appears as its own step, so one
failure aborts neither the rest nor the round. A sub-call naming `batch` or a
session-ending tool is refused. The model gets back
`{results: [{tool, ok, result | error}, …]}`.

## Web search

`[search]` names a SearXNG instance and turns on `web_search` for talk,
check-in and nightly sessions; without it no session is offered the tool. A
call runs the search, then one tool-less model call on the `search` prompt that
answers the caller's `question` from the hits alone, citing them by number. The
session sees `{summary, sources}`, or the top five hits raw when the summarizer
cannot answer. Each search logs one `web_search` row with the hit count and
which of the two it was, never the query.
