# HTTP API

Routes live in `server/src/api.rs` unless noted. Every route takes the session
cookie from `POST /api/login`; the routes listed under
[API tokens](security.md#api-tokens) also take `Authorization: Bearer note_…`.
Errors carry `{"error": …}`. `GET /healthz` answers `ok`.

| Area | Routes | Docs |
|---|---|---|
| Account | `POST /api/login`, `POST /api/logout`, `POST /api/password` (ends the account's other sessions), `GET /api/me` (also records where the user is, for share visits), `POST /api/onboarding/done`, `POST /api/presence` | [security.md](security.md) |
| Invites | `GET/POST /api/join/{token}` (`server/src/invites.rs`) | [security.md](security.md#invites) |
| Settings | `GET/PUT /api/settings` | [configuration.md](configuration.md#usertoml-and-the-settings-api) |
| Prompts | `GET/PUT/DELETE /api/prompts/{name}` | [agent.md](agent.md#prompts) |
| Tasks | `GET/POST /api/tasks`, `PATCH/DELETE /api/tasks/{id}`, `POST /api/tasks/{id}/split`, `POST /api/tasks/{id}/flatten`, `GET /api/tasks/queue` (open tasks ranked), `GET /api/tasks/candidates` (today's run order, then the queue) | [planning.md](planning.md#tasks) |
| Run order | `GET/PUT /api/order` | [planning.md](planning.md#run-order) |
| Import | `PUT/DELETE /api/tasks/by-external/{external_id}`, `PUT/DELETE /api/calendar/by-external/{external_id}`, `POST /api/tasks/{id}/agent`, `POST /api/agent/inbox` | [importing.md](importing.md) |
| Inbox | `GET /api/inbox`, `GET /api/inbox/{id}`, `POST /api/inbox/refresh` (`server/src/inbox.rs`) | [configuration.md](configuration.md#servertoml) |
| Goals | `GET/POST /api/goals`, `PATCH/DELETE /api/goals/{id}` | |
| Plan | `GET /api/plan/today[?calendar=1]`, `GET /api/plan/range` (existing plans across a span; never generates one), `GET /api/day/{date}` (plan, calendar, free time, and what already happened), `POST /api/plan/{date}/carry` (moves what is left of a day to tomorrow) | [planning.md](planning.md#the-daily-plan) |
| Events | `POST /api/events/{id}/{shift,snooze,done,drop,alert,move_tomorrow}` | [planning.md](planning.md#the-daily-plan) |
| Letters | `GET /api/debrief[?date]` (the nightly debrief), `GET /api/review[?week]` (the weekly letter, written by the Monday nightly) | |
| Work sessions | `POST /api/sessions`, `GET /api/sessions/open`, `GET /api/sessions/today`, `POST /api/sessions/{id}/{end,pause,resume,step,skip_break}` | |
| Calendar | `GET/POST /api/calendar`, `PATCH/DELETE /api/calendar/{id}`, `POST /api/calendar/{id}/skip`, `DELETE /api/calendar/{id}/skip/{date}`, `GET /api/calendar/day/{date}` | [planning.md](planning.md#calendar-api) |
| Agent | `POST /api/talk`, `GET /api/conversations`, `PATCH/DELETE /api/conversations/{id}`, `GET /api/conversations/{id}/messages` | [agent.md](agent.md#sessions) |
| Memory | `GET /api/memory`, `GET /api/memory/{id}` | [agent.md](agent.md#memory) |
| Delivery | `GET /api/ws`, `GET /api/push/vapid_public_key`, `POST /api/push/subscribe`, `POST /api/push/unsubscribe`, `POST /api/notify/test` | [delivery.md](delivery.md) |
| Voice | `POST/DELETE /api/voice/link`, `POST /api/voice/test`, `GET /api/voice/voices`, `GET /api/voice/preview`, `GET /api/call/ws` (a web call) | [voice.md](voice.md#server-side) |
| Tokens | `GET/POST /api/tokens`, `DELETE /api/tokens/{id}` | [security.md](security.md#api-tokens) |
| Share links | `/api/shares…` (owner), `/api/share/{token}…` (visitor) | [share-links.md](share-links.md) |
| Security | `/api/security…` (`server/src/security.rs`) | [security.md](security.md#second-factors) |
| Admin | `/api/admin/…` (`server/src/admin.rs`) | [security.md](security.md#admin-routes) |

Opening a work session stops whatever was running and lays the first progress
check itself.
