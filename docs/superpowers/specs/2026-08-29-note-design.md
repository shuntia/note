# Note — Design Spec

2026-08-29

## What it is

Note is a self-hosted accountability and notification system for people with
ADHD. It externalizes working memory: tracking fragmented tasks, scheduling the
day, and proactively reaching out so the user never has to remember to consult
it. A household server runs everything; household members connect via clients.

## Principles

- **Guilt-free by design.** Responses are one tap or one sentence. No streaks,
  no badges, no scolding. Follow-up on a missed check-in is the agent's judgment
  call, governed by editable prompt files — never a hardcoded escalation ladder.
  Retention is the product: if interacting feels like a chore or a judgment,
  the system has failed.
- **YAGNI, swappable guts.** Thin implementations behind small interfaces.
  Every unit replaceable without surgery on its neighbors.
- **Tool calls bound the AI.** Agents have no filesystem or shell access.
  Every capability is a typed tool call the server validates and executes.
- **No model in the plumbing.** Context assembly, scheduling, and delivery are
  code. Models plan, converse, and decide follow-ups — nothing else.
- **Configurable everything.** Providers, tone, schedules, channels — all in
  editable config files. Wide geographic/persona range (students, parents,
  timezones, locales) is a config concern, not a code concern.

## Deployment model

Server–client. One server per household (admin-operated), a handful of user
accounts. Clients in v1: a web PWA. Native iOS is a later phase, built
around no paid Apple Developer account: scheduled local notifications from a
synced plan feed; no APNs.

## Interaction model

Three tiers of outreach, by intrusiveness:

1. **Nudge** — push notification (Web Push in v1).
2. **Call** — a real phone call: server dials via telephony provider (Twilio),
   audio bridged to the speech-to-speech model. The phone genuinely rings;
   works on any phone, no app required.
3. **User-initiated talk** — the user opens a text or voice conversation with
   the LLM whenever they want.

Two loops:

- **Nightly** — while the user sleeps, the agent builds tomorrow's plan from
  the day template, calendar imports, and open tasks, and writes a debrief of
  the past day. Both are delivered together as one morning day-start artifact.
- **Daytime** — a cron-like runner fires the plan's events (check-in calls,
  nudges). Timing has wiggle room: events carry flexibility attributes and can
  be slid, snoozed, or dropped at runtime by the agent or the user.

## Stack

- **Server:** Rust — axum + tokio, WebSockets via tokio-tungstenite. Ships as
  one binary serving the API and the built web assets. Minimal footprint,
  explicit validation, clear errors.
- **Tool schemas:** serde + schemars — single source of truth per tool: the
  same Rust type deserializes (and rejects) incoming calls and generates the
  JSON Schema handed to the LLM.
- **Storage:** SQLite via rusqlite (bundled), plain SQL behind a small DAO
  layer.
- **Scheduler:** small custom runner; a cron crate for expressions only.
- **Channels:** `web-push` crate (VAPID); Twilio via its REST API + Media
  Streams WebSocket — no SDK dependency.
- **Web:** React + Vite TypeScript PWA — the only place npm/pnpm appears,
  build-time only; output is static files served by the server binary.
- **Testing:** cargo test; proptest for property-based tests and fuzzing.
- **Dev environment:** host rustup toolchain (via nix-ld) for day-to-day
  work; `flake.nix` devshell (cargo, rustc, node, pnpm) as the reproducible
  build path for fresh checkouts.

## Architecture

Monorepo: `server/` (Rust), `web/` (TS React PWA), `ios/` (later phase).
Single server process, SQLite storage. Units:

### API layer
REST + WebSocket for clients; serves the PWA. Session login per household
member; admin role gates config and user management.

### Scheduler
- **Day template** (per-user config): recurring skeleton — wake, work/school
  blocks, meals, sleep — timezone-aware, weekday variants.
- **Day plan**: one generated instance per user per day. A list of events:
  time, type (check-in call, nudge, task block, debrief delivery), linked
  tasks, flexibility (fixed / slideable ± window / droppable), channel
  preference.
- **Runner**: fires events when due. Shift operations (slide one, slide rest
  of day, snooze, drop) are first-class, callable by agent tool and UI tap.
  They mutate the plan instance, never the template.

### Tasks
Independent of days: id, LLM-enriched description, state (open / in-progress /
done / dropped), subtasks, source (conversation, quick-add, calendar,
reminder), agent-maintained notes. Plans reference tasks; unfinished tasks
roll forward for the next nightly run to reconsider.

Capture paths, all LLM-mediated: conversation (agent extracts and files
tasks), quick-add (one line from the phone, enriched by the LLM using known
context), calendar import (CalDAV/Google — fixed commitments to schedule
around). Apple Reminders sync: only if trivial, later phase.

### Agent runtime
Runs the nightly plan+debrief job, in-day follow-up decisions, and
conversations. Persona, tone, planning instructions, and follow-up policy live
in per-user `prompts/*.md` — behavior changes are file edits, not deploys.
Each session type gets a fixed tool registry; a check-in call sees a smaller
surface than the nightly planner. Core tools:

- `memory_query`, `memory_read(id)`, `memory_write` (add / update / supersede)
- `context_edit` (targeted edit of the standing document)
- task ops (create, update state, annotate)
- schedule ops (slide, snooze, drop, insert event)
- outreach ops (send nudge, place call) where the session type warrants

The server validates every call; models supply content, never paths.

### Memory layer
Per-user store following the knowit pattern, implemented in-process
(multi-user), borrowing from the knowit indexer source where it fits, and
file-compatible with knowit's format:
`memory/<user>/{semantic,episodic,procedural,archive}/` — one fact per
markdown file, frontmatter with one-line `summary`, supersede-never-delete
with archive moves, episodic→semantic consolidation over time. A derived
SQLite index provides hybrid retrieval: lexical always; vector when an
embeddings provider is configured, degrading to lexical-only when absent or
down. The agent does a memory pass after each session: search first, then
NOOP / ADD / UPDATE / SUPERSEDE. Files on disk are for the admin's eyes;
models reach memory only through tools.

### Context injection
Pure code, no model call, two layers ordered for prompt-cache stability:

1. **Standing context document** (`standing.md`, per user) — active tasks with
   content and progress, important notes, standing preferences. The agent
   edits it in place via `context_edit`; it is never regenerated wholesale.
2. **Dynamic state block** — rendered from the DB at session start: current
   time, today's schedule with live statuses, recent events (completions,
   misses, last check-in summaries).

Injected into every session of every kind — Realtime session instructions for
S2S, system-prompt segment for text LLMs. Deep-archive facts are pulled on
demand via `memory_query`, not prepended.

### Provider layer
- `LLMProvider` — Anthropic API and any OpenAI-compatible endpoint (DeepSeek
  etc.).
- `SpeechProvider` — OpenAI Realtime-compatible S2S.
- `EmbeddingsProvider` — OpenAI-compatible, optional.

Each configured with base URL, model, and key env-var name. A mock
implementation of each ships for deterministic tests; the system is fully
buildable and testable with no live tokens.

The deployment host (`shuntia-nix`) runs a llama.cpp router at
`http://localhost:8080/v1` (OpenAI-compatible, models loaded on demand). Its
`embeddinggemma` is the default `EmbeddingsProvider` — hybrid memory search
works from day one. Its chat models are too weak or too slow for production
agent quality; they serve only as an integration-test backend. Deterministic
tests use the mocks; real agent quality waits on cloud tokens.

## Deployment target

NixOS (`shuntia-nix`), 16 cores, 16 GB RAM, NVMe. One Rust binary + static
web assets + SQLite files, run as a systemd service. Toolchain and web build
come from the repo's `flake.nix` devshell.

### Channel layer
`Channel` interface: `deliver(user, message, urgency)`. v1 implementations:

- **WebPush** — VAPID to the installed PWA.
- **Voice** — Twilio call, Media Streams bridged to the speech provider.
- **WebSocket** — in-app delivery when a client is connected.

The iOS local-notification feed is a later fourth implementation.

### Storage
SQLite: users/sessions, tasks, day plans and event state, memory index,
event log. Memory entries and all config are files on disk.

## Configuration

One `config/` directory, hot-reloaded where cheap:

- `config/server.toml` — bind address, base URL, auth secrets location,
  provider registry, channel credentials (Twilio, VAPID), storage paths, log
  level.
- `config/defaults/` — shipped defaults; per-user files override by key, so a
  new household member is one directory copy.
- `config/users/<user>/`
  - `user.toml` — identity, timezone, locale, phone number, quiet hours,
    channel preferences, escalation appetite, day-template selection.
  - `templates/*.toml` — day templates, weekday variants.
  - `prompts/*.md` — persona, tone (guilt-free rules live here), planning
    instructions, follow-up policy.
  - `standing.md` — standing context document.

## Web client (v1)

Installable PWA: today view (plan + task states; tap to shift / snooze /
done), talk view (text + WebRTC voice to the S2S bridge), quick-add box,
debrief screen, admin pages (config editing, user management).

## Error handling

- Scheduler, UI, and delivery keep running when providers are down — they are
  code.
- Nightly run falls back to "copy template + roll tasks forward," so there is
  always a morning plan.
- Failed calls fall back to web push.
- Every degradation is logged and surfaced in the admin UI; the user sees no
  errors, only a plainer day.

## Testing

The tool layer gets the heaviest investment: it is the sole boundary between
model output and system state, and a validation gap there is state corruption
with no second line of defense.

- **Tool fuzzing (first-class):** every tool is fuzzed with proptest —
  malformed JSON, wrong types, boundary values, oversized payloads, unknown
  fields, path-like and injection-shaped strings. Required outcome: a typed
  rejection returned to the model; never a throw, never a partial write.
- **Invariant properties:** arbitrary interleaved sequences of valid and
  invalid tool calls, asserting state invariants after every step — memory ids
  unique and matching filenames, supersede targets exist, archives never
  edited, plan events reference real tasks, template never mutated by shift
  ops. Each tool call is transactional: it fully applies or leaves no trace.
- **Registry enforcement:** per-session-type tool registries tested
  negatively — a check-in session calling a planner-only tool is rejected.
- Unit: scheduler and shift logic, memory store semantics (supersede,
  archive, consolidation), context assembly, config overlay resolution.
- Provider layer: tested against the mock LLM/S2S/embeddings implementations;
  the mock LLM doubles as a fuzzer driver, emitting pathological tool calls.
- Integration: one test driving a full simulated day — nightly run, morning
  delivery, check-ins, shifts, memory pass.

## Phases

1. **v1 (this spec, buildable on Linux):** server + PWA; mock providers;
   Web Push; Twilio voice bridge; scheduler, tasks, memory, agent runtime.
2. **Live providers:** real tokens dropped into config; prompt tuning.
3. **iOS native:** local-notification plan sync, nicer talk UX; built on a
   Mac with free provisioning (AltStore/SideStore auto-refresh from the
   server); CallKit only if an Apple Developer membership materializes.
4. **If easy:** Apple Reminders sync.
