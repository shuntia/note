# Note Chat & Settings Overhaul Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild Talk into a llama.cpp-webui-class chat (persisted conversations, visible tool calls, markdown), add a real per-user settings surface (timezone, nightly time, display name, template, theme), and gate member creation to the server CLI.

**Architecture:** Conversations and messages move into SQLite so Talk survives reloads and replays history to the agent. `run_session` reports every tool call as a structured step; the talk endpoint persists and returns them. Settings read/write the existing `config/users/<name>/user.toml` merge source atomically, so the runner/nightly pick changes up on their next tick with no reload machinery. The web client gets a two-pane chat referencing the llama.cpp webui layout (sidebar conversations, markdown messages, collapsible tool-call blocks, autosizing composer) rendered in Note's existing dawn design language.

**Tech Stack:** Existing server stack (axum 0.8, rusqlite, jiff 0.2). Web: React 19 + Vite 7 TS-strict; NEW bundled runtime deps `marked` (^16) and `dompurify` (^3) — the only two allowed.

**Spec:** User directive of 2026-08-30 (this plan's authority): chat must reference the llama.cpp webui (sidebar + chat + composer, proper tool-call display), settings must stop being sparse (timezones configurable), adding a member must be CLI-only. Reference for the llama.cpp webui feature set: conversation sidebar (new/rename/delete), markdown with code-copy, collapsible reasoning/tool blocks, settings dialog, dark/light theming.

## Global Constraints

- `cargo test` green; `cargo clippy --all-targets` shows exactly the two accepted warnings (`result_large_err`, `type_complexity`) and no others.
- `pnpm build` (tsc strict + vite) green in `web/`; `marked` and `dompurify` are the ONLY new runtime deps; zero external runtime resources (no CDN, system font stacks only).
- Assistant-authored HTML is NEVER injected unsanitized: every markdown render passes through DOMPurify.
- API errors: JSON `{"error": "..."}` bodies; cross-user access is indistinguishable from absence (404); unauthenticated is 401.
- All timestamps stored as RFC3339 UTC via `jiff::Timestamp` (matches existing tables).
- Existing dawn palette tokens in `web/src/styles.css` are the design system; dark scheme via the existing token override blocks; respect `prefers-reduced-motion`.
- Comments only where the signature can't speak; no process-history narration.
- DB access always via the `Mutex<Connection>` lock, never held across a provider call (existing invariant).

---

### Task 1: Conversation storage + CRUD API

**Files:**
- Modify: `server/src/db.rs` (append ONE migration to `MIGRATIONS`)
- Create: `server/src/talk.rs`
- Modify: `server/src/lib.rs` (add `pub mod talk;`)
- Modify: `server/src/api.rs` (routes + handlers)
- Test: `server/tests/conversations_api.rs`, unit tests in `talk.rs`

**Interfaces:**
- Produces (Task 2 relies on these exact signatures):
  - `talk::create(conn: &Connection, user_id: i64, title: &str, now: jiff::Timestamp) -> anyhow::Result<i64>`
  - `talk::owned(conn: &Connection, user_id: i64, id: i64) -> anyhow::Result<bool>`
  - `talk::touch(conn: &Connection, id: i64, now: jiff::Timestamp) -> anyhow::Result<()>` (bumps `updated_at`)
  - `talk::append_text(conn: &Connection, conversation_id: i64, role: &str /* "user"|"assistant" */, content: &str, now: jiff::Timestamp) -> anyhow::Result<()>`
  - `talk::append_tool(conn: &Connection, conversation_id: i64, tool_name: &str, tool_args: &str, result: &str, is_error: bool, now: jiff::Timestamp) -> anyhow::Result<()>`
  - `talk::history(conn: &Connection, conversation_id: i64, limit: usize) -> anyhow::Result<Vec<crate::providers::Message>>` — the last `limit` `user`/`assistant` rows (tool rows excluded), returned oldest-first as `Message::User(..)` / `Message::Assistant { text, tool_calls: vec![] }`
  - `talk::title_from(message: &str) -> String` — first line-ish: whitespace runs collapsed to single spaces, trimmed, truncated to ≤ 60 chars on a char boundary with `…` appended when truncated
- Migration (append as a new `&str` at the END of `MIGRATIONS`; `PRAGMA foreign_keys=ON` is already set, so the CASCADE is live):

```sql
CREATE TABLE conversations (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id),
    title TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX idx_conversations_user ON conversations(user_id, updated_at DESC);
CREATE TABLE talk_messages (
    id INTEGER PRIMARY KEY,
    conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('user','assistant','tool')),
    content TEXT NOT NULL,
    tool_name TEXT,
    tool_args TEXT,
    is_error INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_talk_messages_conv ON talk_messages(conversation_id, id);
```

For `tool` rows, `content` holds the tool RESULT (JSON text as returned to the model); `tool_name`/`tool_args` hold the call. For text rows both tool columns are NULL.

**Routes** (all under the existing session auth; handlers follow the lock-scope style of `tasks_list`):
- `GET /api/conversations` → `200 [{"id":1,"title":"…","updated_at":"…"}]`, newest `updated_at` first.
- `PATCH /api/conversations/{id}` body `{"title": "…"}` → 200 on success; 400 `{"error":"title must be non-blank and at most 120 characters"}` (trim first); 404 when absent/unowned.
- `DELETE /api/conversations/{id}` → 200; 404 when absent/unowned. Single `conn.execute` on `conversations` (CASCADE removes messages).
- `GET /api/conversations/{id}/messages` → 200 ordered by `id`: `[{"id":7,"role":"tool","content":"{…}","tool_name":"task_create","tool_args":"{…}","is_error":false,"created_at":"…"}, …]` (tool columns `null` on text rows); 404 when absent/unowned.

**Steps:**
- [ ] Migration + `talk.rs` unit tests first: `title_from` (short passthrough, 60-char truncation on a multibyte boundary, whitespace collapse), `history` (excludes tool rows, honors limit taking the LAST N, maps roles, oldest-first), CASCADE delete removes messages.
- [ ] Run `cargo test` — new tests fail; implement; green.
- [ ] Integration tests in `conversations_api.rs` using the existing `common` helpers: empty list; rename round-trip reflected in list; blank/121-char title → 400; delete removes from list AND its messages (seed via `talk::append_text`); other user's conversation → 404 for rename/delete/messages; unauthenticated → 401.
- [ ] `cargo test` green; clippy baseline unchanged.
- [ ] Commit: `feat: persisted conversations with CRUD api`

---

### Task 2: Session steps, history replay, talk endpoint rework

**Files:**
- Modify: `server/src/agent.rs`, `server/src/nightly.rs` (caller), `server/src/api.rs` (talk handler)
- Test: extend `server/src/agent.rs` unit tests, `server/tests/talk_api.rs`

**Interfaces:**
- Consumes: all `talk::*` from Task 1.
- Produces (Task 4's client types mirror this wire shape):

```rust
#[derive(Debug, Clone)]
pub struct SessionStep {
    pub name: String,
    pub args: String,
    pub result: String,
    pub is_error: bool,
}
// SessionOutcome gains: pub steps: Vec<SessionStep>
pub fn run_session(
    deps: &SessionDeps,
    user_id: i64,
    username: &str,
    kind: SessionKind,
    now: jiff::Timestamp,
    history: &[Message],
    opening: &str,
) -> Result<SessionOutcome>
```

`messages` starts as `history.to_vec()` then pushes `Message::User(opening)`. Each dispatched tool call pushes one `SessionStep` (same strings that go into `Message::ToolResult`). `nightly.rs` passes `&[]`.

**Talk endpoint contract** (`POST /api/talk`):
- Request: `{"message": "…", "conversation_id": 3}` — `conversation_id` optional.
- With a `conversation_id` that is absent/unowned → 404 BEFORE entering the talk gate.
- History: under the lock, load `talk::history(conn, conv_id, 32)`; pass to `run_session`.
- On `Ok(out)`: one lock scope persists everything in order — create the conversation if the request had none (`talk::create` with `talk::title_from(&message)`), `append_text` the user message, `append_tool` each step in order, `append_text` the assistant reply (after the existing `EMPTY_REPLY_FALLBACK` substitution — persist what the user sees), `talk::touch`. Then respond `{"conversation_id": id, "reply": "…", "steps": [{"name":"task_create","args":"{…}","result":"{…}","is_error":false}, …]}`.
- On session error: existing 502/500 paths unchanged; NOTHING persisted (no conversation, no user row — the client keeps the draft).
- Existing 400/409/503 gate behavior, `talk_error` logging, and spawn_blocking structure unchanged.

**Steps:**
- [ ] Extend agent unit tests: the round-trip test asserts `out.steps == [SessionStep{name:"task_create", args:…, result contains "task_id", is_error:false}]`; the rejected-tool test asserts a step with `is_error:true`; new test: `history` passed in is visible to the model (`seen()[0].messages` starts with the history) and the opening lands last.
- [ ] Implement; `cargo test` green.
- [ ] `talk_api.rs` integration tests (MockLLM scripted like agent.rs tests): reply carries `conversation_id` + `steps`; DB now holds user/tool/assistant rows in order with the right roles and columns; a second POST with that `conversation_id` replays history (assert via a scripted provider that records message counts — reuse the `app_with_logged_in_user_llm_and_state` passthrough); unowned/absent `conversation_id` → 404 and nothing persisted; failed session (FailingLLM) persists nothing; blank-reply session persists the fallback string.
- [ ] `cargo test` green; clippy baseline unchanged.
- [ ] Commit: `feat: talk sessions persist history and surface tool steps`

---

### Task 3: Settings API + CLI-gated membership

**Files:**
- Modify: `server/src/api.rs` (routes `GET/PUT /api/settings`; REMOVE `/api/admin/users` route + `admin_create_user` handler)
- Modify: `server/src/config.rs` (settings write helper) and `server/src/context.rs` (make `write_atomic` `pub(crate)`)
- Modify: `server/src/auth.rs` (`validate_username` back to private if no longer referenced outside)
- Modify: `README.md` (membership is CLI-only; settings endpoints)
- Test: `server/tests/settings_api.rs`; update `server/tests/admin_api.rs` (or wherever admin-user-creation tests live) — creation route must now 404

**Interfaces:**
- `GET /api/settings` → 200:

```json
{
  "display_name": "X", "timezone": "Asia/Tokyo", "nightly_time": "03:00", "template": "default",
  "templates": ["default", "deep-work"],
  "timezones": ["Africa/Abidjan", "…"]
}
```

Effective values come from `UserConfig::load` (defaults merged with the user file). `templates` = sorted, deduped `.toml` file stems from `config/defaults/templates/` plus `config/users/<name>/templates/`. `timezones` = sorted names from `jiff::tz::db().available()`.
- `PUT /api/settings` body: any subset of `{"display_name","timezone","nightly_time","template"}`. Validation (400 with a field-naming `{"error":…}` on the first failure): `display_name` trimmed non-empty ≤ 64 chars; `timezone` accepted by `jiff::tz::TimeZone::get`; `nightly_time` parses as `%H:%M` (reject `24:00`); `template` must be in the `templates` list. On success merge into current effective values and write `config/users/<username>/user.toml` atomically (temp file + rename via `context::write_atomic`) containing exactly the four keys as TOML, then 200 with the same body shape as GET (minus the two list fields). Concurrency: do the read-merge-write while holding the DB lock guard as a cheap serializer (settings writes are rare; note the ruling in a declaration comment).
- Member creation: ONLY `cargo run -- create-user <name> <password> [--admin]`. The route, handler, and its tests are deleted; `GET /api/admin/log` stays admin-gated and untouched.

**Steps:**
- [ ] Tests first in `settings_api.rs`: GET shape (seeded defaults file → effective values + non-empty timezones list including "UTC" and the seeded template); PUT timezone+nightly_time → 200 and a following GET reflects it AND the user.toml file on disk contains the four keys; each invalid field → 400 naming the field; unauth → 401; `POST /api/admin/users` → 404 (was 200/403).
- [ ] Implement; run; green. Update README ("Members are created on the server CLI, not over HTTP" + settings docs).
- [ ] `cargo test` green; clippy baseline unchanged.
- [ ] Commit: `feat: user settings api; member creation is cli-only`

---

### Task 4: Web data layer for conversations, steps, settings

**Files:**
- Modify: `web/src/types.ts`, `web/src/api.ts`

**Interfaces (produced — Tasks 5/6 consume verbatim):**

```ts
export type Conversation = { id: number; title: string; updated_at: string }
export type TalkStep = { name: string; args: string; result: string; is_error: boolean }
export type TalkMessage = {
  id: number; role: 'user' | 'assistant' | 'tool'; content: string;
  tool_name: string | null; tool_args: string | null; is_error: boolean; created_at: string
}
export type TalkReply = { conversation_id: number; reply: string; steps: TalkStep[] }
export type Settings = {
  display_name: string; timezone: string; nightly_time: string; template: string;
  templates: string[]; timezones: string[]
}
```

`api` gains: `conversations(): Promise<Conversation[]>`, `renameConversation(id, title)`, `deleteConversation(id)`, `conversationMessages(id): Promise<TalkMessage[]>`, `talk(message, conversationId?): Promise<TalkReply>` (replaces the old signature), `settings(): Promise<Settings>`, `saveSettings(patch: Partial<Pick<Settings,'display_name'|'timezone'|'nightly_time'|'template'>>): Promise<void>`. REMOVE `adminCreateUser`. Follow the existing typed endpoint-map style and `ApiError` handling exactly.

**Steps:**
- [ ] Implement; `pnpm build` green (Talk.tsx/More.tsx may need the minimal call-site adjustments to keep tsc green — keep them compiling, their redesign lands in Tasks 5/6).
- [ ] Commit: `feat: web api layer for conversations, steps, settings`

---

### Task 5: Talk view — llama.cpp-class chat (frontend-design task)

**Files:**
- Modify: `web/package.json` (add `marked` ^16, `dompurify` ^3 — pnpm), `web/src/views/Talk.tsx` (full rewrite), `web/src/styles.css` (chat styles)
- Create: `web/src/markdown.tsx` (render helper + message component)

**Interfaces:**
- Consumes Task 4's `api`/types exactly.
- `markdown.tsx` exports `Markdown({ text }: { text: string })` — parses with `marked` (gfm on, headers/lists/code/links/blockquotes; `async: false`), sanitizes with DOMPurify (`ALLOWED_URI_REGEXP` default; add `target="_blank" rel="noreferrer"` to links post-sanitize), renders via `dangerouslySetInnerHTML` inside a `.prose` container. Fenced code blocks get a copy button (event delegation on the container; `navigator.clipboard.writeText`; button label flips to "copied" for 1.5s).

**Binding requirements (WHAT, not pixel-level HOW — the frontend-design skill owns the visual execution, in the existing dawn token language):**
- Two-pane layout ≥ 880px: conversation sidebar (New chat; list newest-first showing title; per-item rename (inline input) and delete (confirm) affordances; active item marked) + chat pane. Below 880px the sidebar collapses behind a toggle in the chat header; selecting a conversation closes it.
- Selecting a conversation loads its messages (loading state, error state with retry). New chat = empty pane + composer; the first reply's `conversation_id` becomes current and the sidebar refreshes.
- Message rendering: user messages as distinct right-side bubbles (plain text, `white-space: pre-wrap`); assistant messages as full-width markdown prose via `Markdown`; historical tool rows and live `steps` render identically: a collapsible block (`<details>`) between messages whose summary line shows a tool glyph, the tool name in mono, and an ok/error badge (clay tokens on error); expanded body shows args and result, each pretty-printed (`JSON.stringify(JSON.parse(x), null, 2)` with a plain-text fallback) in scrollable mono blocks.
- Composer: autosizing textarea (1→8 rows), Enter sends / Shift+Enter newline, send button disabled while blank/busy; while waiting show a "Note is thinking…" pending row (subtle animation, disabled under `prefers-reduced-motion`); on error surface the API error text inline as a system row, KEEP the draft in the textarea.
- Scroll: message pane owns the scroll (page body never scrolls); stick to bottom on new content unless the user has scrolled up.
- The 409 "reply already in progress" and 502 bodies from the server render as readable system rows, not toasts.
- Dark scheme must hold (tokens only, both `data-theme` and `prefers-color-scheme` paths); `pnpm build` green.

**Steps:**
- [ ] `pnpm add marked dompurify` (workspace root `web/`); build `markdown.tsx`; then the view + styles.
- [ ] Verify with `pnpm build`; self-review against every binding requirement above.
- [ ] Commit: `feat: talk is a persisted chat with visible tool calls`

---

### Task 6: More → Settings surface (frontend-design task)

**Files:**
- Modify: `web/src/views/More.tsx`, `web/src/styles.css`, `web/src/main.tsx` (pre-paint theme), `web/src/app.tsx` (only if the masthead needs `display_name`)

**Binding requirements:**
- REMOVE the Add-a-member card entirely (membership is CLI-only).
- New Settings card, top of the view: display name (text), timezone (text input + `<datalist>` of `settings.timezones` — typeable search), nightly time (`<input type="time">`), template (`<select>` from `settings.templates`). Explicit Save button with busy state, success confirmation, and the server's 400 `error` text shown verbatim on failure. Values load from `GET /api/settings` (loading + retry states like DebriefCard).
- Appearance card: theme segmented control System / Light / Dark → sets `data-theme` on `document.documentElement` (removed attribute = System) and persists to `localStorage` (guard reads/writes with try/catch); `main.tsx` applies the stored choice before React mounts so there is no flash.
- Keep, restyled coherently with the new cards: Debrief, Notifications (push toggle), Server log (admin only), Sign out.
- Dark scheme holds; `pnpm build` green.

**Steps:**
- [ ] Implement; `pnpm build`; self-review against the requirements.
- [ ] Commit: `feat: settings surface with timezone, schedule, theme`

---

### Task 7: Visual verification loop (controller task — no dispatch)

Controller rebuilds (`pnpm build`), restarts the server, drives the headless-chromium CDP script through: login → Today → Talk (real NIM exchange that triggers a tool call; verify the tool block renders and expands; reload page; verify history persists; new chat; sidebar ops) → More (save a timezone change; verify GET reflects it; flip theme) — capturing screenshots at each step. Findings become fix rounds against the responsible task's implementer (or controller-applied for trivial diffs, ledgered).

---

### Task 8: Docs + final review

- [ ] README "Web client" + Talk/API sections describe conversations, steps, settings; config-layout comment mentions nothing stale (admin creation reference removed everywhere — `grep -ri "add a member\|admin/users" README.md docs/ web/ server/`).
- [ ] Commit: `docs: chat, settings, cli-gated membership`
- [ ] Final whole-branch review (Fable) per subagent-driven-development; ONE fix wave; merge via finishing-a-development-branch.
