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
- `models`: more models a user may pick, beside `model`, from the composer's
  "+" menu (one chat) or Settings (their default), each
  `{ id = "<provider id>", name = "<shown name>", desc = "<a few words>" }`;
  listing `model` itself gives it a name too. A session asks for the
  chat's pick, else the user's default, else `model`; the chat's or the user's
  effort (`off`, `low`, `medium`, `high`) likewise overrides `reasoning`.
  Voice calls and share-link visitors always run on the server's own. A pick
  later dropped from the list falls back to `model`.
- `cache_ttl_min`: how long the endpoint keeps a prompt cached; it caps how
  long a conversation may idle before the summary pass (`[agent]
  idle_summary_min`) replays it.

`[providers.embeddings]` (OpenAI-compatible, e.g. a local llama.cpp router)
makes memory search hybrid (lexical + vector); it falls back to lexical when
the provider is down.

## Sessions

Session kinds: talk (`POST /api/talk`), trigger (a wake-up, below),
nightly, review, summarize, task briefing and inbox reading
([importing.md](importing.md)), share-link visitor
([share-links.md](share-links.md)), and voice calls ([voice.md](voice.md)).

`POST /api/talk {message, conversation_id?}` runs one turn. Omit
`conversation_id` and the server opens a conversation titled from the message;
pass one and its last 32 text turns are replayed first. The reply carries
`conversation_id`, the reply text, `steps` (every tool call with arguments,
result, error flag, and `thinking`, the reasoning of the round that made it, on
the round's first call), `reasoning` for the answering round, and `thought_ms`,
the wall clock from the first provider call to the reply. Progress streams over
the WebSocket ([delivery.md](delivery.md#websocket)). A turn still running
after 45 seconds answers `202 {conversation_id, turn, pending: true}` and ends
with a `talk_done` frame; `GET /api/talk/live` replays the frames of the
user's turn in flight. Each tool call's row is written as its result comes.

`POST /api/talk/stop {conversation_id?}` stops the user's turn (in that thread
when given) and answers `204` once it has ended, also when nothing ran. The
model stream and the tool loop end at once; what the turn did and the reply
streamed so far are kept, marked stopped. A turn held up past 3 seconds in a
provider call or a tool is let go: its slot is freed so the next message
runs, and nothing it does afterwards lands in the thread.

Guards keep a turn from spinning. A write that changed nothing (refused, or
everything `unchanged`) is not progress, and the same write sent again is
answered from before; two in a row get a note to fix the arguments, and four
rounds without progress, one of them such a write, make the next round the
reply. A round's output is capped at 8192 tokens (16384 with reasoning), a
streamed round is cut past 32 KiB of reasoning or 3 minutes, and a talk turn
past 150k completion tokens; each makes the next round the reply.

Conversations: `GET /api/conversations`, `PATCH/DELETE /api/conversations/{id}`,
and `GET /api/conversations/{id}/messages` (user, assistant and tool rows in
order; each carries `reasoning`, and the assistant row `thought_ms`).

### Models

`GET /api/models` answers `{default, models, efforts, effort_default}`: the
provider's live model (null on the mock), the models on offer as
`{id, name, desc}` (that one first, then `[providers.llm] models`), `["off","low","medium","high"]`, and the
configured effort (null when none is set). `PUT /api/conversations/{id}/model
{model, effort}` sets a thread's own pick (null follows the user's default);
a value not on offer is a `422`, another user's thread a `404`. Each listed
conversation carries its `model` and `effort`. `POST /api/talk` may carry
`model` and `effort` for the thread it opens. A session asks for the thread's
pick, then the user's (`model`, `effort` in Settings), then the provider's;
only talk turns read a thread's pick, and calls and share visitors always run
on the provider's own.

`GET /api/triggers` is the signed-in user's trigger log, oldest first: up to
100 settled in the last 7 days, then up to 50 still waiting, each
`{event_id, date, at, status, prompt, origin, outcome, text}` with `outcome`
one of `said`, `rang`, `quiet`, `cancelled`, `replaced` or null. With
`show_triggers` on, Chat lists it as a read-only thread.

## Nightly run

At each user's `nightly_time` (default 03:00, their timezone) the server
rewrites the standing strategy on Mondays, generates the day's plan from the
template, and runs the nightly session: it adjusts the plan, sets the day's run order
([planning.md](planning.md#run-order)), settles due working notes, writes the
morning debrief, and last leaves itself `nightly_notes.md`. If the model is
unreachable the plan still exists and a fallback debrief says so. The nightly
run also archives expired memory facts. One `nightly_run` row records each
stage's outcome and timing.

## Wake-ups

A wake-up is a trigger session: Note looks at the day on its own and ends with
`say` or `stay_quiet`. Four things lay one (all but `wake_save` only with
check-ins on):

- `lay_day`, once per local day, at the user's first activity at or after
  05:00, or at `day_start` (default 08:00) if they have not shown up by then; a
  blank `day_start` turns it off, and nothing is laid at or after
  `close_day_time`. Its session checks the order, sets it if the night left it
  empty or wrong, and lays 4 to 8 wake-ups with `wake_save`. An undecided one
  is retried once.
- `wake_save`, from any session that has it, for any time still ahead on any
  later day; a far day's plan waits bare (the wake-up alone) until that day is
  generated from the template in force then. These come out of the day's
  `triggers_per_day` (default 12, 0 to 20); `wake_save {extra_wakes: {n,
  reason}}` raises one day's allowance, to at most twice it, once the user
  agrees in a chat. One at the same minute and thread as a waiting one is
  refused, naming it; one within 20 minutes is laid with a warning naming the
  other. Wake-ups laid today within a work session's planned length plus 15
  minutes (any time today when it named none) belong to the session and do not
  count until it holds 8, its own checks included; dropped ones never count. At
  most 200 wait at once across all days. A wake-up that ends in plain text
  instead of a tool says that text. The context lists today's and tomorrow's
  waiting wake-ups; later ones only as a count and the next one's moment.
- Idle: after `idle_nudge_min` (default 20; 0 is off) minutes without a sign of
  the user on a day they were active, outside a work session, quiet window and
  `close_day_time`, while the agent's own wake-ups leave budget. Idle checks
  never spend the budget themselves. Each idle check that ends quiet doubles the
  wait for the next (20, 40, then 80 minutes) until the user is seen again, and
  none is laid within an hour of one that spoke while nothing has changed since
  (no message, task change or plan event). Its opening says how long the user has been
  quiet and lists active working notes, if any, with each one's age and last
  nudge.
- Tidy-ups (origin `chores`), at each of `chores_times` (default 11:00, 15:00,
  19:00) after `day_start` and before `close_day_time`, off the budget. The
  nightly run lays them for its day and every sweep lays today's still ahead,
  so a time is laid once per day whatever became of it; a pending one whose
  time the user dropped is taken back. One waits out an open work session
  (dropped once 90 minutes late) and never folds with other wake-ups. Its
  opening asks it to merge duplicate tasks, re-date or drop plainly dead ones
  (asking when unsure), add obvious durations and steps, and supersede, fix or
  remove memories, then offer one small task and end with a short `say`. It
  lists up to 25 task candidates (likely duplicates, overdue 3+ days, untouched
  14+ days, 90+ minute roots with no steps, steps with no duration), a random
  small step, the next 30 long-term facts after a per-user cursor
  (`memory_rounds`, wrapping), and what earlier tidy-ups said today. It cannot
  call, a question in it waits only when it names `await_min`, and any
  wake-up it lays is dropped when it ends.

A wake-up tends tasks, the order, the schedule, working notes and memory
([tools](#tools)), lays tasks on the plan, reads goals and the calendar, searches
the web where search is configured, and drops a task the user no longer
wants, but never deletes a task or writes goals or the calendar.
Each successful task, note, day, memory or wake-up write logs one
`wake_change` row (`event <id>: <tool> <args>`, args clipped to 300
characters). It may not edit the task of a running work session, that task's
parent or its steps.

A wake-up ends in one of three ways: it messages the user, calls them, or
stays quiet (`stay_quiet {reason}`). Any session that lays a trigger can leave
it to call; a chat that is asked "call me at three" lays a
`wake_save {ring: true}` whose note says a call was asked for.

`say {text, notes?}` lands in the trigger's thread (or the day's check-in
thread) and goes down the delivery ladder; `notes` names the working notes it
nudged about. With `await_min` (5 to 120) the session parks instead of
ending (`paused_sessions`, on an `await_reply` event in the same thread) and
resumes with `Timed out`, `User responded before timeout` (as soon as the user
writes anywhere) or `User acted before timeout` (a task finished or changed, a
work session started or ended), and `changed`, what moved in the context since.
A day holds at most 6 such waits. On a trigger laid with `ring: true` (its
opening says so), `say` rings instead when the [gate](delivery.md#ringing)
lets a call through, and waits on the call as a confirmed `call_user` does.
When the gate holds it, it is sent as a message and waits for an answer
(`await_min`, else 20 minutes), its result carrying `call: "Held: <why>"`.

`call_user` rings the user ([delivery.md](delivery.md#ringing)) from every
session but a voice call, a share link and the single-call kinds (import,
inbox, summarize). It takes two calls in one session. The first,
`{reason, asked?, quote?}`, rings nothing: it refuses (`busy`) when nothing can
ring, a call is live, or the user seems busy (inside a quiet window, inside a
`fixed` or `busy` calendar occurrence, with a work session open, or outside
the hours they take calls) or set `ring_for = "never"`: the same [gate](delivery.md#ringing)
a trigger laid to ring passes. Otherwise it returns a
token valid for five minutes, an instruction to prepare (anticipate what the
call will cover and fetch it now, in one round), and how to write a spoken
opening. `asked` with the user's words in `quote` lets it through the busy
check only, and only when the quote is part of a message the user wrote that
the session holds (a chat's own turns, or the thread a wake-up was laid in);
nothing gets past `ring_for = "never"`. The second, `{token, opening, brief}`,
rings; `brief` (at most 1536 bytes) is what the session gathered, in short
plain lines. A wake-up's second call parks it and its firing path rings,
writing the opening to the trigger's thread and `trigger_rang`. The session
waits for nothing but the call's outcome (or 45 minutes, past the longest live
call) and resumes with `Answered` and `on_the_call` (the last 12 lines said on
it), `Missed`, `Declined`, or `Failed` with `reason` when nothing could ring,
plus `changed`. The wait counts against the day's 6; past them the call rings
and the session ends. Any other
session writes the opening to the day's check-in thread and logs
`call_user_rang`. The call's prompt is told the reason, the opening and the
brief.

## Prompts

Agent behaviour lives in `config/defaults/prompts/*.md` (`ja/` for Japanese)
and belongs to the app: users cannot replace it, so every user gets each
change. Changing policy is a file edit, not a deploy. `persona` drives chat,
`import` task briefing, `inbox` item reading, `share` share-link sessions,
`search` the web-search summarizer, `planning` the nightly run, `trigger`
wake-ups, `voice` calls.

What a user writes about themselves (Settings → About you,
`config/users/<user>/about.md`, at most 8 KiB) follows the shipped prompt
under "About the user, in their own words" in every session but share, task
briefing, inbox and summarize. Who they are and how Note should talk to them
goes there.

- `GET /api/about` → `{content}`, empty when there is none.
- `PUT /api/about {content}` saves it trimmed; blank content clears it.
- `GET /api/debug/prompt?kind=talk|trigger|nightly|call` → `{prompt, tools}`:
  the system prompt and tool names a session of that kind would start with
  now (Settings → Debug).

## Context

Every session's system prompt but share, task briefing, inbox and summarize
carries, in order:

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
   nightly run; today's plan, only what is still pending, snoozed or fired, the
   current and next waiting item marked and every status counted; today's and
   tomorrow's waiting wake-ups apart (`18:57 e844 "note" · ring · cancel on
   reply`); Now tasks with their steps and the Later list (at most ten titles,
   led by the nearest deadline), each line carrying its deadline; how much is
   due inside three days; what was finished today; today's run order (the
   first 12 items); `# Working memory`, below; the latest debrief in
   excerpt; whether tomorrow is planned; settings that shape advice and the
   live memory-fact count; the last ten user-meaningful `event_log` rows
   (deliveries, sessions, tokens, admin and security rows left out). Titles,
   states, durations and step titles only: descriptions and notes never reach
   the prompt. The block is held under 6 KiB plus the working memory by
   shortening the Later list, then the debrief, then the activity tail, and
   last night's notes last; the real-time line, Now tasks, plan and working
   memory are never trimmed. Anything still over is cut at a whole line, closed
   with how many lines were left out. Ids read as the tools take them: `t1103`
   for a task, `e844` for a plan item or wake-up.

## Working memory

Notes are Note's own scratchpad, never shown to the user: one-line memory files
of category `note` under `data/memory/<user>/note/`, at most 160 characters,
with an optional `from`/`until` window. Every session that gets the context
block sees the titles of the notes active now (at most 40, newest first) and
of those opening within 12 hours, each with its id. Memory search, the memory
tools and routes, and the fact count leave notes out.

`note_save {title, from?, until?, id?}` (or a short `items` list) writes one,
or with `id` rewords it or moves its window (an empty `from` or `until`
clears it); a new one titled like an open one is that note. `note_remove
{ids}` deletes. Times are RFC 3339 or a local `YYYY-MM-DDTHH:MM` or
`YYYY-MM-DD`.

A note is due once past its `until`, or, with none, three days after it was
last kept, written or opened. The nightly run lists due notes; `memory_save`
with the note's id and a kind turns one into a long-term fact (episodic for a
windowed note, semantic otherwise), `note_remove` drops it, and every due note
left afterwards is kept as a fact word for word.

## Memory

Per-user long-term memory lives under
`data/memory/<user>/{semantic,episodic,procedural,archive}/`: one markdown fact
per file, frontmatter with a one-line summary (`note/` beside them holds
working memory). Facts are never deleted:
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
| `task_find`, `goal_find`, `calendar_find`, `day_find` | talk, nightly, trigger, call, share |
| `task_save` | talk, nightly, trigger, call |
| `task_remove`, `goal_save`, `goal_remove`, `calendar_save`, `calendar_remove` | talk, nightly, call |
| `day_save`, `day_remove`, `wake_find`, `wake_save`, `wake_remove` | talk, nightly, trigger, call |
| `memory_find` | talk, nightly, trigger, call, inbox |
| `memory_save` | talk, nightly, trigger, call, review |
| `memory_remove`, `note_save`, `note_remove` | talk, nightly, trigger, call |
| `context_edit` | talk, nightly |
| `timezone_set` | talk, call |
| `call_user` | talk, nightly, trigger |
| `nightly_notes_write` | nightly |
| `web_search` | talk, nightly, trigger, call (with `[search]`) |
| `batch` | talk, nightly, trigger |
| `work_end`, `say`, `stay_quiet` | trigger |
| `cancel_job`, `hang_up` | call |
| `task_brief` / `inbox_decide` / `summary_write` / `review_write` / `share_note` | import / inbox / summarize / review / share, each ending its session on success |

Details:

- Writes answer `{ok, saved, adjusted, ignored, refused, warnings}`; an empty
  save is refused with an example of the list it takes.
- `day_*` hold task items, routines and calendar events; a wake-up sent there
  is refused with a pointer to `wake_*`. `day_find` only counts the wake-ups
  waiting; `wake_find` lists every one ahead with its id, note, ring and
  `cancel_on` (`reply`, `task` or `event`).
- `memory_*` hold long-term facts; a working note sent there is refused with a
  pointer to `note_*`.
- `task_save` creates or updates a nested tree; a new task titled like an open
  one elsewhere is made with a warning naming it, unless `new: true`.
- Retired names (`task_update`, `trigger_set`, `note_write`, …) answer with the
  tool that does their work now.

`batch` runs 1 to 25 of those calls in one model round. Each sub-call takes the
path and transaction it would take alone and appears as its own step, so one
failure aborts neither the rest nor the round. A sub-call naming `batch` or a
session-ending tool is refused. The model gets back
`{results: [{tool, ok, result | error}, …]}`.

## Web search

`[search]` names a SearXNG instance and turns on `web_search` for talk,
wake-up, nightly and call sessions; without it no session is offered the tool. A
call runs the search, then one tool-less model call on the `search` prompt that
answers the caller's `question` from the hits alone, citing them by number. The
session sees `{summary, sources}`, or the top five hits raw when the summarizer
cannot answer. Each search logs one `web_search` row with the hit count and
which of the two it was, never the query.
