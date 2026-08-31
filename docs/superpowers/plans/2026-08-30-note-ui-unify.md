# Note UI Unification + Memory & Prompts Surfaces Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the web client a unified, full-width app shell in the llama.cpp webui design language, add read-only Memory browsing and system-prompt (persona) configuration end to end.

**Architecture:** Three small read-mostly server APIs (memory list/get, prompts get/put/delete) reuse existing `memory.rs`/`prompts.rs` internals. The web client gets a rebuilt design-token layer and app shell (desktop left sidebar, mobile bottom tabs, full-width content), then each view is restyled or added on top of that foundation.

**Tech Stack:** Rust (axum 0.8, rusqlite), React 19 + Vite 7 + TS strict. No new dependencies on either side.

**Spec:** `docs/superpowers/llamaui-design-reference.md` (committed alongside this plan) is the binding design authority for all web tasks. Screenshots referenced there live in the session scratchpad under `llamaui-shots/` (ephemeral; the doc's descriptions are self-sufficient).

## Global Constraints

- Web client: NO new npm dependencies (runtime deps stay exactly react, react-dom, marked, dompurify); zero external network resources at runtime.
- `cd web && pnpm build` (tsc strict + vite) must pass after every web task.
- `cargo test` green after every server task; `cargo clippy --all-targets` stays at exactly the 2 accepted warnings (result_large_err, type_complexity).
- All new API routes take `user: CurrentUser` (401 gating comes free); memory and prompt data is per-user by construction — never accept a username from the client.
- Prompt name whitelist is exactly `["persona", "planning"]`; anything else is 404 before touching the filesystem.
- Comments only at declarations that aren't self-evident (CLAUDE.md policy). No process-history narration anywhere.
- Theme must keep working three ways: `data-theme="dark"`, `data-theme="light"`, and unset + `prefers-color-scheme` (both blocks in styles.css stay in sync), and the pre-paint script in `web/index.html` keeps matching the storage key `note.theme`.
- Dark palette follows the design doc's soft-dark numbers; `--sun` stays the single accent.

---

### Task 1: `memory::list`

**Files:**
- Modify: `server/src/memory.rs`

**Interfaces:**
- Consumes: existing index table + `QueryHit { id, category, summary }` (already `pub`, already serializable via the API layer building JSON by hand — check; if `QueryHit` lacks `serde::Serialize`, derive it here).
- Produces: `pub fn list(conn: &Connection, user: &str, category: Option<&str>, limit: usize) -> anyhow::Result<Vec<QueryHit>>` — newest first (highest rowid first in the index), active facts only (the index only holds non-archived facts, same source `lexical_query` reads), optional exact category filter. `limit` caps rows; callers pass a sane value.

- [ ] **Step 1: Write failing tests** in `memory.rs`'s existing `#[cfg(test)] mod tests`, following the module's existing test setup helpers (it already has tests that create a temp data dir + in-memory conn and call `add`):

```rust
#[test]
fn list_returns_newest_first_and_filters_category() {
    // setup mirrors the module's existing add/query tests
    let (conn, tmp) = test_env(); // reuse/extract whatever helper the existing tests use
    let a = add(&conn, tmp.path(), "aki", "semantic", "fact a", "body a", None).unwrap();
    let b = add(&conn, tmp.path(), "aki", "episodic", "fact b", "body b", None).unwrap();
    let c = add(&conn, tmp.path(), "aki", "semantic", "fact c", "body c", None).unwrap();

    let all = list(&conn, "aki", None, 50).unwrap();
    assert_eq!(all.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), vec![c.as_str(), b.as_str(), a.as_str()]);

    let sem = list(&conn, "aki", Some("semantic"), 50).unwrap();
    assert_eq!(sem.len(), 2);
    assert!(sem.iter().all(|h| h.category == "semantic"));

    assert_eq!(list(&conn, "aki", None, 1).unwrap().len(), 1);
    assert!(list(&conn, "nobody", None, 50).unwrap().is_empty());
}

#[test]
fn list_excludes_archived() {
    let (conn, tmp) = test_env();
    let old = add(&conn, tmp.path(), "aki", "semantic", "old", "old body", None).unwrap();
    supersede(&conn, tmp.path(), "aki", &old, "new", "new body", None).unwrap();
    let ids: Vec<String> = list(&conn, "aki", None, 50).unwrap().into_iter().map(|h| h.id).collect();
    assert!(!ids.contains(&old));
}
```

Adjust the exact `add`/`supersede` call shapes to the real signatures in the file — the test intent (ordering, category filter, limit, cross-user isolation, archived exclusion) is the requirement.

- [ ] **Step 2:** `cargo test memory::` — new tests FAIL (no `list`).
- [ ] **Step 3:** Implement `list` as a single indexed SELECT (same table `lexical_query` uses), `WHERE user = ?` plus optional `AND category = ?`, `ORDER BY rowid DESC LIMIT ?`.
- [ ] **Step 4:** `cargo test` — all green; clippy baseline unchanged.
- [ ] **Step 5:** Commit `feat: memory::list for browsing`.

### Task 2: Memory read API

**Files:**
- Modify: `server/src/api.rs` (router + two handlers), `README.md` (short "Memory API" subsection under "Memory & agent tools")

**Interfaces:**
- Consumes: `memory::list` (Task 1), `memory::query(conn, user, q, limit, None)`, `memory::read(data_dir, user, id)`, `memory::valid_id`.
- Produces routes:
  - `GET /api/memory?category=&q=&limit=` → `{"items": [{"id", "category", "summary"}]}`. With `q` non-blank: `memory::query` (lexical arm, `query_vec` None), ignoring `category`. Without `q`: `memory::list`. `limit` default 100, clamped to 1..=200. `category` outside `semantic|episodic|procedural` → 400 `{"error":"unknown category"}`.
  - `GET /api/memory/{id}` → full fact `{"id","category","summary","body","supersedes","created","archived"}`; invalid id (fails `valid_id`) or missing → 404.

- [ ] **Step 1: Failing tests** in `api.rs`'s test module, following its existing route-test harness (app + login helper already exist there):

```rust
#[tokio::test]
async fn memory_routes_list_search_and_read() {
    // seed two facts for the logged-in user directly via memory::add
    // GET /api/memory → both, newest first
    // GET /api/memory?category=semantic → only the semantic one
    // GET /api/memory?q=<word in one summary> → that one
    // GET /api/memory/<id> → body + summary present
    // GET /api/memory/nope → 404; GET /api/memory/../etc → 404
    // unauthenticated GET /api/memory → 401
}

#[tokio::test]
async fn memory_routes_are_per_user() {
    // fact seeded for user A is invisible to logged-in user B (empty list, 404 on direct id)
}
```

Write these as real tests with the module's established helpers and exact asserts.

- [ ] **Step 2:** Run — FAIL (404 route not found).
- [ ] **Step 3:** Implement handlers `memory_list` and `memory_read`; wire `.route("/api/memory", get(memory_list))` and `.route("/api/memory/{id}", get(memory_read))`.
- [ ] **Step 4:** `cargo test` green; clippy baseline unchanged.
- [ ] **Step 5:** README subsection (≤15 lines) documenting both routes.
- [ ] **Step 6:** Commit `feat: read-only memory API`.

### Task 3: Prompts API

**Files:**
- Modify: `server/src/api.rs`, `server/src/prompts.rs`, `README.md` (extend the prompt-files paragraph with the routes)

**Interfaces:**
- Consumes: `prompts::load(config_dir, user, name)`, `context::write_atomic`.
- Produces in `prompts.rs`:
  - `pub const EDITABLE: [&str; 2] = ["persona", "planning"];`
  - `pub fn custom(config_dir: &Path, user: &str, name: &str) -> bool` (user override file exists)
  - `pub fn save(config_dir: &Path, user: &str, name: &str, content: &str) -> anyhow::Result<()>` (create dirs, write via `write_atomic`)
  - `pub fn reset(config_dir: &Path, user: &str, name: &str) -> anyhow::Result<()>` (remove override; Ok if absent)
- Produces routes:
  - `GET /api/prompts/{name}` → `{"name","content","custom"}` (content = effective via `load`)
  - `PUT /api/prompts/{name}` body `{"content"}` (deny_unknown_fields) → validates: trimmed non-empty, ≤ 32768 bytes → 400 `{"error":...}` otherwise; writes override; returns same shape as GET with `"custom": true`
  - `DELETE /api/prompts/{name}` → removes override, returns GET shape (now `"custom": false`, default content)
  - Any `{name}` not in `EDITABLE` → 404 for all three verbs.

- [ ] **Step 1: Failing tests** — unit tests in `prompts.rs` for `custom`/`save`/`reset` (save then load returns new content; reset restores default; reset when absent is Ok), plus an `api.rs` route test:

```rust
#[tokio::test]
async fn prompts_routes_get_put_delete() {
    // GET persona → default content, custom=false
    // PUT persona {"content":"you are testy"} → 200, custom=true; GET reflects it
    // PUT with blank content → 400; PUT with 40_000 bytes → 400
    // DELETE persona → custom=false, default content again; DELETE again → still 200
    // GET /api/prompts/evil → 404; PUT /api/prompts/../x → 404
    // unauthenticated → 401
}
```

(The api test harness must point the app's config_dir at a temp dir seeded with `defaults/prompts/persona.md` — mirror how existing settings tests seed config.)

- [ ] **Step 2:** Run — FAIL.
- [ ] **Step 3:** Implement prompts.rs fns, then handlers `prompt_get`/`prompt_put`/`prompt_delete` + route `.route("/api/prompts/{name}", get(prompt_get).put(prompt_put).delete(prompt_delete))`.
- [ ] **Step 4:** `cargo test` green; clippy baseline unchanged.
- [ ] **Step 5:** README: add the three routes to the prompt-files paragraph.
- [ ] **Step 6:** Commit `feat: per-user prompt override API`.

### Task 4: Design tokens + unified full-width app shell

**Files:**
- Modify: `web/src/styles.css` (token layer + shell/nav/login/toast/shared control styles), `web/src/app.tsx` (shell markup), `web/index.html` (only if the theme-color meta needs a new token name — the pre-paint script itself must not change semantics)
- Everything else keeps rendering (views may look transitional until Tasks 5–8; that is acceptable *within this task only*).

**Interfaces:**
- Consumes: `docs/superpowers/llamaui-design-reference.md` §Palette and §Translating-to-Note (binding).
- Produces (later tasks rely on these exact names):
  - CSS tokens on `:root` + both dark paths: `--bg`, `--surface` (card/raised), `--surface-2` (popover/menu), `--text`, `--text-muted`, `--border`, `--border-input`, `--accent` (the sun orange), `--accent-fg`, `--danger`, `--ring`, `--sidebar-bg`, `--sidebar-border`, `--radius` (0.625rem), `--radius-lg`, `--radius-xl`. Keep `--sun/--moss/--clay/--mono/--sans/--serif` working (alias or retain) so untouched views don't break mid-phase.
  - Shell classes: `.shell` (full-width flex row, 100dvh), `.sidebar` (desktop left nav), `.sidebar-item`, `.content` (flex-1, min-width 0, owns scrolling), `.tabs` (mobile bottom bar, hidden ≥768px while `.sidebar` hides <768px).
  - `app.tsx`: `Tab` type becomes `'today' | 'tasks' | 'chat' | 'memory' | 'settings'`; NAV list `[Today, Tasks, Chat, Memory, Settings]`; `<Talk/>` renders under `chat`, `<More/>` under `settings`; `memory` renders a placeholder `<section className="pane" />` until Task 7. Masthead moves into the sidebar header (word "Note", serif) with username + date beneath; mobile keeps a slim top header.

Design requirements (from the reference, binding): light bg near-white with white surfaces; dark = soft dark (bg ≈ `oklch(0.16 0 0)`, surfaces ≈ `oklch(0.205 0 0)`, borders = translucent white 10–30%); monochrome neutrals, `--sun` reserved for primary buttons, active nav item, focus rings; sidebar items = icon-less text rows, rounded-lg, active = accent-tinted fill; content constrained per-view, not by the shell.

- [ ] **Step 1:** Rewrite the token block (all three theme paths) and shell layout; update `app.tsx` markup.
- [ ] **Step 2:** `cd web && pnpm build` — green.
- [ ] **Step 3:** Visual smoke: served build or vite dev via headless chromium at 1280×900 and 420×860, light + dark; screenshot each nav destination; confirm sidebar/tabs swap at 768px and no horizontal scroll anywhere.
- [ ] **Step 4:** Commit `feat: full-width unified shell with llama-ui token layer`.

### Task 5: Chat restyle

**Files:**
- Modify: `web/src/views/Talk.tsx`, `web/src/styles.css` (chat section), `web/src/markdown.tsx` (only if code-block chrome needs class changes)

**Interfaces:**
- Consumes: Task 4 tokens/classes; existing Talk state machine (era-scoped pending, msgState) — behavior must not change, this is a restyle.
- Produces: chat layout per design doc §anatomy: conversation sidebar list restyled onto `--sidebar-bg` tokens; message column centered `max-width: 48rem` inside full-width pane; user turns as right-aligned muted bubbles (max-width 80%, radius ~1.25rem); assistant turns unbubbled on the background; tool-call `<details>` styled as `--surface` cards (rounded-lg, 1px border, mono summary line); composer as detached rounded (1.5rem) `--surface` card with borderless textarea, circular accent send button (↑), `shadow-sm`→focus `shadow-md`; thinking indicator uses a shimmer-style muted text (CSS only, reduced-motion aware).

- [ ] **Step 1:** Restyle; keep every existing className hook the tests/CDP scripts rely on (`.turn.user`, `.turn.assistant`, `details`, `.chat-toggle`) working.
- [ ] **Step 2:** `pnpm build` green.
- [ ] **Step 3:** Headless visual pass: empty chat, sent message (against live server), expanded tool block, mobile drawer — light + dark screenshots.
- [ ] **Step 4:** Commit `feat: llama-ui chat styling`.

### Task 6: Today & Tasks full-width layouts + debrief on Today

**Files:**
- Modify: `web/src/views/Today.tsx`, `web/src/views/Tasks.tsx`, `web/src/views/More.tsx` (remove debrief card), `web/src/styles.css`

**Interfaces:**
- Consumes: Task 4 shell; existing `api.debrief()` call currently in More.
- Produces: Today = responsive two-column ≥1024px (time-spine plan left ~2/3, morning debrief card right ~1/3, stacking below), content capped `max-width: 72rem`, event rows as `--surface` cards with the action buttons as ghost buttons that fill on hover; Tasks = same cap, quick-add as a composer-style rounded input row, task rows full-width cards with checkbox styling per design doc §controls. Debrief card moves from More to Today (markdown rendering as-is); More loses it without other regressions.

- [ ] **Step 1:** Implement both views + debrief move.
- [ ] **Step 2:** `pnpm build` green.
- [ ] **Step 3:** Headless screenshots 1280 + 420, light + dark.
- [ ] **Step 4:** Commit `feat: full-width today and tasks`.

### Task 7: Memory view

**Files:**
- Create: `web/src/views/Memory.tsx`
- Modify: `web/src/app.tsx` (replace placeholder), `web/src/api.ts`, `web/src/types.ts`, `web/src/styles.css`

**Interfaces:**
- Consumes: Task 2 routes; Task 4 shell; `markdown.tsx` renderer for fact bodies.
- Produces:
  - `api.ts`: `memoryList(params: {category?: string; q?: string}) => Promise<{items: MemoryHit[]}>`, `memoryRead(id: string) => Promise<MemoryFact>`; `types.ts`: `MemoryHit {id; category; summary}`, `MemoryFact {id; category; summary; body; created; archived; supersedes: string | null}`.
  - View: master-detail (list left ~22rem, detail right, single-pane drill-down <768px with a back button). Toolbar: search input (debounced 300ms → `?q=`) + category filter chips (All/Semantic/Episodic/Procedural, hidden while a search is active). List rows: summary line + small category chip. Detail: summary as title, category + created date muted meta line, body rendered as markdown. Empty states: "No memories yet — the assistant saves what it learns as you talk." / "Nothing matches." Errors surface via the existing `notify` toast; read-only view, no write UI.

- [ ] **Step 1:** Implement view + api/types.
- [ ] **Step 2:** `pnpm build` green.
- [ ] **Step 3:** Headless pass against live server seeded with a few facts (seed via `memory_write` tool trigger or direct file + reindex ruling — implementer may seed by calling the server's talk endpoint or by writing files and restarting; document which in the report). Screenshots: list, search hit, detail, mobile drill-down, light + dark.
- [ ] **Step 4:** Commit `feat: memory browser view`.

### Task 8: Settings master-detail + persona editor

**Files:**
- Modify: `web/src/views/More.tsx` (rename to `web/src/views/Settings.tsx` with `git mv`, update import in `app.tsx`), `web/src/api.ts`, `web/src/types.ts`, `web/src/styles.css`

**Interfaces:**
- Consumes: Task 3 routes; Task 4 shell; existing settings/push/log/theme logic (behavior preserved).
- Produces:
  - `api.ts`: `promptGet(name) => Promise<PromptDoc>`, `promptPut(name, content) => Promise<PromptDoc>`, `promptReset(name) => Promise<PromptDoc>`; `types.ts`: `PromptDoc {name; content; custom}`.
  - Layout: left section list (Profile, Schedule, Appearance, Persona, Notifications, Admin — Admin only for `me.is_admin`), right scrollable pane, llama-ui field pattern (label / control / one-line muted helper under each). Sections regroup EXISTING controls: Profile = display name; Schedule = timezone, nightly time, template; Appearance = theme trio; Notifications = push toggle; Admin = server log. Save semantics unchanged (existing PUT /api/settings flow, per-section save button where fields exist). Sign-out stays visible (sidebar footer of the section list).
  - Persona section: select between Persona and Planning prompt; textarea (mono, min 16 rows) loading `promptGet`; "Customized" badge when `custom`; Save → `promptPut` (disabled while unchanged/blank); "Reset to default" → confirm dialog (centered rounded-xl per design doc) → `promptReset`; helper text explains this is the assistant's system prompt.
  - Mobile: section list collapses to a horizontal chip row above the pane.

- [ ] **Step 1:** Implement restructure + persona editor.
- [ ] **Step 2:** `pnpm build` green.
- [ ] **Step 3:** Headless pass: each section, persona edit→save→custom badge→reset roundtrip against live server, light + dark, 1280 + 420.
- [ ] **Step 4:** Commit `feat: settings master-detail with persona editor`.

---

## Controller verification (after Task 8, before final review)

Not a dispatched task. Against the real server + NIM from the worktree: full walkthrough at 1280/420 × light/dark; live talk turn asking the agent to remember a fact → confirm `memory_write` fires (steps block) and the fact then appears in the Memory view; persona roundtrip changes a live reply's tone. Send representative screenshots to the user.

## Final review & merge

Whole-branch review (most capable model) with ONE fix wave, then superpowers:finishing-a-development-branch: full `cargo test` + `pnpm build`, ff-merge to main, verify on main, clean up worktree + branch.
