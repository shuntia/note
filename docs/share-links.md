# Share links

A user can hand someone they trust a link to a chat with Note about a chosen
slice of their day, with no account on the visitor's side.

## Owner routes

Minted in Settings → Advanced → Share links, or over the session:

| Route | Effect |
|---|---|
| `POST /api/shares {name, brief, scope, expires_at}` | the row plus `url` |
| `GET /api/shares` | every link with `url`, `messages_today`, thread count, `visitors`, `distant_visits` |
| `PATCH /api/shares/{id}` | name, brief, scope or expiry |
| `DELETE /api/shares/{id}` | revokes |
| `GET /api/shares/{id}/threads` | every visitor conversation |
| `GET /api/shares/{id}/visits` | every opening, newest first, as `{city, country, km, distant, at}` |

A write passes only with `Sec-Fetch-Site` `same-origin`, `none`, or absent;
anything else is `403`. Names are 1–64 characters and a brief at most 4 KiB
(`422`). An expiry in the past is `422`; one beyond `share_max_days` is pulled
back to it. A user at `shares_per_user` links gets `409`. A `PATCH`'s `scope`
is a whole object: switches left out take their defaults.

In Settings a link's URL shows once made; *Copy link* and *Preview as visitor*
work while it lives; *Activity* lists visits and threads; *Revoke* confirms in
a toast. A link opened from far away carries a rose dot.

## Scope

`{today, tasks, categories, goals, progress, details, horizon_days, notes, messages_per_day}`:

- `today`: the plan and calendar for `horizon_days` (1 to 14) days from today.
  Calendar titles travel whenever it is on.
- `tasks`: task reads.
- `categories`: empty means all; otherwise every task read, goal count and plan
  block is confined to them.
- `details`: off means titles only; descriptions, notes, `url` and
  `external_id` are withheld, and search matches titles alone.
- `goals`: off also withholds the goal a task hangs from.
- `progress`: off, the tools reach live tasks alone; on, tasks done in the last
  7 days too. Never older ones, never dropped ones.
- `notes`: the visitor may leave a message that reaches the owner's channels as
  "Note from <link name>"; filing it ends the turn and the visitor reads
  "Passed on to <owner>."
- `messages_per_day`: visitor messages across all the link's threads in any
  24 hours (`429` beyond it), at most `share_messages_per_day`.

## Visitor routes

The visitor opens `/s/<token>`, a chat headed by the owner's name and one line
on what is shared and until when.

- `GET /api/share/{token}`: the header data; each call is one visit.
- `POST /api/share/{token}/messages {message, thread?}`: one turn. The reply
  names its `thread`, which the page sends back to continue; without one a new
  thread starts, so every page load begins fresh and old history is never
  re-sent to the model.
- `GET /api/share/{token}/messages?thread=<id>`: a thread back.
- `GET /api/share/{token}/view`: the slice as data.

A cookie marks the visitor: a thread answers only to the cookie that started it
(`404` otherwise), and distinct cookies are what `visitors` counts. Every route
is `404` once the link expires, is revoked, or its owner is disabled. A stored
scope that no longer parses fails the request rather than widening it.

Unknown-token lookups and message posts are limited per client address
(`cf-connecting-ip`, then the first `x-forwarded-for` hop): 60 in 15 minutes,
then `429`.

Visit location comes from Cloudflare's visitor location headers (`cf-ipcity`,
`cf-ipcountry`, `cf-iplatitude`, `cf-iplongitude`; the zone's *Add visitor
location headers* managed transform). Distance is measured from where the
owner was last seen, which `GET /api/me` records from the same headers. A visit
farther than `share_distant_km` is distant; one without coordinates on either
side is never marked.

## The session

Its own session kind with read-only tools: `task_list`, `task_search`,
`task_read` when `tasks` is on; `plan_list` and `calendar_list` when `today`
is; `goal_list` when `goals` is; `share_note` when `notes` is. `plan_list` and
`calendar_list` refuse dates outside the horizon. The owner's trigger points
are left out entirely; a block for a task outside the shared categories is
`{"kind":"busy","start","end","status"}`, and every other block carries
`task_title`.

Its system prompt is the `share` prompt (editable under Settings → Advanced →
How Note talks), the per-link brief, and a fresh rendering of the scope on
every message. It never sees the standing context, memory, chat threads or
settings. Turns log as `share_session`, outside the owner's
`agent_sessions_per_day` and outside the activity block the owner's sessions
read.

Limits live in `[limits]` ([configuration.md](configuration.md#servertoml)).
