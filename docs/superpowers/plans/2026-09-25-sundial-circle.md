# Sundial Circle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Today face becomes the circle: tap to start work on the most pressing task, swipe to switch task or to plain work time, tap to pause, swipe down to finish; rounds are beads, breaks drain the arc in sage, and the phone is told when a session ends.

**Architecture:** Three server additions (task queue, rounds-today + discard, end-of-session notices) on the existing session and channel code. On the web, a motion module `fx.ts` ported from the mockup's `fx.js`, a pure-helper module `circle.ts` (geometry, gesture arithmetic, hint counters), and the Home view's face rewritten around one gesture surface. No new dependencies.

**Tech Stack:** Rust (axum, rusqlite, jiff) under `server/`; React 19 + TypeScript + Vite + GSAP 3.15 under `web/` (pnpm, vitest).

**Spec:** `docs/superpowers/specs/2026-09-25-sundial-design.md` section E. Motion reference: `docs/superpowers/mockups/sundial/fx.js` and `flow.html` (numbers there win over prose).

## Global Constraints

- Repo `CLAUDE.md`: no comments narrating history; comment only what the code cannot say.
- Reduced motion: every GSAP helper returns early under `prefers-reduced-motion: reduce`; the track and fill simply appear, the ring closes with a 200 ms opacity change, the strip snaps without tweening.
- No words for state: no eyebrow, no "n of m", no "focused minutes", no urgency badges on the face.
- Colours: `--arc-sun: oklch(86% 0.18 84)`, `--arc-sage: oklch(76% 0.15 150)`, used only by the arc, beads and ripples.
- Geometry (SVG viewBox 320, y down): r 148, C = 2π·148 = 929.9, SPAN = C·240/360 = 619.9, the arc starts at `rotate(150)` and runs clockwise; the opening is the bottom 120°, from 30° to 150°.
- Server times are RFC 3339 UTC; the user's local day comes from `user_zone(&state, &user.username)`.
- Every commit message ends with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.
- `cargo test -p note-server` after each server task, `cd web && pnpm exec tsc --noEmit && pnpm test` after each web task; all green before committing.

## Review Focus

1. A swipe that lands on a slot within 60 s of the last start must discard, not end, so the day's history holds no 20-second stubs (Task 12 test; Task 15 uses `discard` by elapsed time).
2. The server flips a pomodoro phase up to 30 s after the client's counter hits 00:00; the face must play the transition once, not twice (Task 16: a `seenPhase` ref keyed by session id + phase).
3. A tap during the draw-and-unwind must not pause or start a second session (Task 15: `settling` flag blocks the tap surface for the sequence's length).
4. A single session with no `planned_min` (Work time) never gets a "Time's up." message (Task 13 test).
5. A user with `session_end_notify` off still sees the in-page transition and the vibration (Task 16 does not read the setting; only the server does).

---

### Task 11: `GET /api/tasks/queue`

**Files:**
- Modify: `server/src/tasks.rs` (add `QueueEntry`, `queue`), `server/src/allocate.rs` (extract `rank_key` from `pack`'s sort, `allocate.rs:120-136`), `server/src/api.rs` (route next to `/api/tasks`, handler beside `tasks_list` at `api.rs:398`)

**Interfaces:**
- Produces: `pub fn queue(conn: &Connection, user_id: i64, now: jiff::Timestamp, limit: usize) -> rusqlite::Result<Vec<QueueEntry>>`, `#[derive(Serialize)] pub struct QueueEntry { pub task: TaskNode, pub step: Option<Task>, pub planned_min: Option<u32>, pub reason: &'static str }`.
- Produces: `allocate::rank_key(is_now: bool, urgency_rank: u8, due: Option<jiff::civil::Date>, created: &str, id: i64) -> (Reverse<bool>, u8, bool, Option<Date>, String, i64)`.

- [ ] **Step 1: Failing tests in `tasks.rs`'s test module**

Use the file's existing helpers for an in-memory db and a user (grep `open_memory` and `create_user` in its tests). Insert five tasks and then `UPDATE tasks SET due_at=?, urgency=?, is_now=? WHERE id=?`:
```rust
#[test]
fn queue_orders_now_then_urgent_then_due_then_oldest() {
    // now: 2026-09-25T20:00:00Z
    // "oldest" (no due), "soon" (due 2026-09-26T09:00Z), "over" (due 2026-09-24T09:00Z),
    // "urgent" (urgency high), "now" (is_now, urgency low)
    // expect ids [now, urgent, over, soon, oldest] and reasons
    // ["now", "urgent", "overdue", "due_soon", "oldest"]
}
#[test]
fn queue_plans_the_step_and_rounds_to_five() {
    // task duration 47 with a first open step of 22 → planned_min 20, step = that child;
    // task duration 47, no steps → 45; neither → None
}
```
Run: `cargo test -p note-server queue_` → FAIL (no `queue`).

- [ ] **Step 2: Implement**

`allocate.rs`: replace the inline comparator in `pack` with `order.sort_by_cached_key(|c| rank_key(c.is_now, c.urgency_rank, c.due, &c.created, c.id))` and
```rust
/// The order work is laid: what the user is on, then urgency, then dated before
/// undated and soonest first, then oldest.
pub fn rank_key(is_now: bool, urgency_rank: u8, due: Option<jiff::civil::Date>, created: &str, id: i64)
    -> (std::cmp::Reverse<bool>, u8, bool, Option<jiff::civil::Date>, String, i64) {
    (std::cmp::Reverse(is_now), urgency_rank, due.is_none(), due, created.to_owned(), id)
}
```
`tasks.rs`:
```rust
pub fn queue(conn: &Connection, user_id: i64, now: jiff::Timestamp, limit: usize) -> rusqlite::Result<Vec<QueueEntry>> {
    let mut nodes: Vec<TaskNode> = list(conn, user_id)?
        .into_iter()
        .filter(|n| matches!(n.task.state.as_str(), "open" | "in_progress"))
        .collect();
    let due_of = |t: &Task| t.due_at.as_deref().and_then(|d| d.parse::<jiff::Timestamp>().ok());
    nodes.sort_by_cached_key(|n| {
        let due = due_of(&n.task).map(|t| t.to_zoned(jiff::tz::TimeZone::UTC).date());
        crate::allocate::rank_key(n.task.is_now, urgency_rank(&n.task.urgency, n.task.pressing), due, &n.task.created_at, n.task.id)
    });
    Ok(nodes.into_iter().take(limit).map(|n| {
        let step = n.children.iter().find(|c| c.state != "done").cloned();
        let minutes = step.as_ref().and_then(|s| s.duration_min).or(n.task.duration_min);
        let overdue = due_of(&n.task).is_some_and(|d| d < now);
        let reason = if n.task.is_now { "now" } else if overdue { "overdue" } else if n.task.urgency == "high" { "urgent" } else if n.task.pressing { "due_soon" } else { "oldest" };
        QueueEntry { task: n, step, planned_min: minutes.map(|m| ((m + 2) / 5 * 5).max(5)), reason }
    }).collect())
}
```
If `Task` has no `created_at`, read it in `list`'s SELECT (it is a column of `tasks`). `pressing` is stamped by `list` with the server's clock; if `list` takes a `now`, pass this one.

`api.rs` handler on `CurrentUser` (never `TaskPrincipal`):
```rust
#[derive(Deserialize)]
struct QueueQuery { limit: Option<usize> }
async fn tasks_queue(user: CurrentUser, State(state): State<AppState>, Query(q): Query<QueueQuery>) -> impl IntoResponse {
    let conn = state.db();
    let tz = user_zone(&state, &user.username);
    let listed = crate::tasks::queue(&conn, user.id, jiff::Timestamp::now(), q.limit.unwrap_or(5).clamp(1, 20))
        .and_then(|mut q| { crate::tasks::stamp_schedule(&conn, user.id, &tz, q.iter_mut().map(|e| &mut e.task.task))?; Ok(q) });
    match listed { Ok(q) => Json(q).into_response(), Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response() }
}
```
Route `.route("/api/tasks/queue", get(tasks_queue))` before `/api/tasks/{id}`. Add a test in `server/tests/` (follow `settings_api.rs`) that a share-token request to `/api/tasks/queue` is refused.

- [ ] **Step 3: Tests pass, commit**

```bash
git add server/src/tasks.rs server/src/allocate.rs server/src/api.rs server/tests
git commit -m "feat(server): the queue of open work in the order the planner lays it

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 12: Rounds today and discard

**Files:**
- Modify: `server/src/work.rs` (add `rounds_today`, `discard`), `server/src/api.rs` (`/api/sessions/today`; `work_session_end` at `api.rs:2182` accepts `discard`)

**Interfaces:**
- Produces: `pub fn rounds_today(conn, user_id, tz: &jiff::tz::TimeZone, now) -> rusqlite::Result<u32>`; JSON `{ "rounds": u32 }`.
- Produces: `pub fn discard(conn, user_id, id, now) -> rusqlite::Result<bool>`: deletes the session (and its conversation row) when `now − started_at < 60 s`, returns whether it did.
- End body: `{ "outcome": "done" | "stopped", "discard"?: bool }`.

- [ ] **Step 1: Failing tests in `work.rs`'s test module (`env_with` helper, `work.rs:689`)**

```rust
#[test]
fn rounds_today_counts_singles_once_and_pomodoros_by_round() {
    // insert: single ended today → 1; pomodoro ended today round 3 → 3; open → 0; yesterday → 0
    // expect 4
}
#[test]
fn discard_removes_only_a_young_session() {
    // start a session at t0, discard at t0+30s → true, row gone
    // start at t0, discard at t0+5min → false, row still open
}
```

- [ ] **Step 2: Implement**

```rust
/// Rounds of work that ended today: one for a single session, `round` for a pomodoro.
pub fn rounds_today(conn: &Connection, user_id: i64, tz: &jiff::tz::TimeZone, now: jiff::Timestamp) -> rusqlite::Result<u32> {
    let (from, to) = crate::day::bounds(now.to_zoned(tz.clone()).date(), tz);  // adapt to day.rs:34-47's real signature
    conn.query_row(
        "SELECT COALESCE(SUM(CASE WHEN mode = 'pomodoro' THEN round ELSE 1 END), 0) FROM work_sessions
         WHERE user_id = ?1 AND ended_at IS NOT NULL AND started_at >= ?2 AND started_at < ?3",
        rusqlite::params![user_id, from.to_string(), to.to_string()],
        |r| r.get::<_, i64>(0),
    ).map(|n| n as u32)
}

/// A session abandoned within its first minute leaves nothing behind.
pub fn discard(conn: &Connection, user_id: i64, id: i64, now: jiff::Timestamp) -> rusqlite::Result<bool> {
    let started: Option<(String, Option<i64>)> = conn.query_row(
        "SELECT started_at, conversation_id FROM work_sessions WHERE id = ?1 AND user_id = ?2 AND ended_at IS NULL",
        rusqlite::params![id, user_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    let Some((started, conversation)) = started else { return Ok(false) };
    let Ok(started) = started.parse::<jiff::Timestamp>() else { return Ok(false) };
    if now.duration_since(started).as_secs() >= 60 { return Ok(false) }
    conn.execute("UPDATE events SET work_session_id = NULL WHERE work_session_id = ?1", [id])?;
    conn.execute("DELETE FROM work_sessions WHERE id = ?1", [id])?;
    if let Some(c) = conversation { crate::chat::delete_conversation(conn, user_id, c)?; }  // use the existing conversation delete; grep `DELETE FROM conversations`
    Ok(true)
}
```
`work_session_end`: extend its body struct with `discard: Option<bool>`; when `discard == Some(true)` and `work::discard` returns true, reply 200 with `{}` and `broadcast_changed`; otherwise fall through to the existing end.

Handler `work_session_today` on `CurrentUser` returning `Json(json!({ "rounds": n }))`; route `/api/sessions/today` beside `/api/sessions/open`.

- [ ] **Step 3: Tests pass, commit**

```bash
git add server/src/work.rs server/src/api.rs
git commit -m "feat(server): rounds done today, and a session abandoned in its first minute leaves no trace

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 13: End-of-session notices

**Files:**
- Modify: `server/src/db.rs` (v40: `ALTER TABLE work_sessions ADD COLUMN end_notified_at TEXT`), `server/src/config.rs` (`session_end_notify: Option<bool>`, accessor default true), `server/src/api.rs` (settings GET/PATCH), `server/src/work.rs` (`tick` honours the setting; new `planned_end`), `server/src/runner.rs:357` (call `planned_end` beside `tick`)

**Interfaces:**
- Produces: `UserConfig::session_end_notify() -> bool`; settings JSON `session_end_notify`.
- Produces: `pub fn planned_end(conn, config_dir, now) -> Result<Vec<Flip>>`: for every open single session with `planned_min`, not paused, `end_notified_at IS NULL`, and `now ≥ started_at + paused_ms + planned_min`, stamps `end_notified_at` and yields a `Flip` whose message is `OutboundMessage { title: session.title, body: "Time's up.", urgency: <the same the flip messages use>, event_id: None, conversation_id: session.conversation_id, .. }` when the user's setting is on, else `None`.

- [ ] **Step 1: Failing tests (`work.rs` tests)**

```rust
#[test]
fn planned_end_notifies_once_and_only_when_wanted() {
    // env_with("") (setting defaults on): start single 25 min at t0; planned_end at t0+24m → empty;
    // at t0+25m → one flip with message; again at t0+26m → empty (stamped)
    // env_with("session_end_notify = false\n"): at t0+25m → one flip with message None, still stamped
}
#[test]
fn work_time_never_gets_a_notice() {
    // start with planned_min None; planned_end at t0+2h → empty
}
#[test]
fn tick_flip_messages_follow_the_setting() {
    // pomodoro session with the setting off: tick at phase end flips and message is None
}
```

- [ ] **Step 2: Implement**

Migration v40 after v39 in `db.rs` (bump the version constant the way v39 did). Config field and accessor like `pomodoro_enabled`. Settings body/patch like `pomodoro_enabled`. In `tick`, where `message: Some(OutboundMessage {...})` is built (`work.rs:567`, `:590`), wrap with `cfg.session_end_notify().then(|| ...)` (the `UserConfig` is already loaded there or load it per user as `start` does). `planned_end` mirrors `overrun`'s SELECT (`work.rs:614-620`) with `AND w.mode = 'single' AND w.end_notified_at IS NULL`. In `runner.rs`, collect `planned_end`'s flips into the same `flips` vector so delivery and `broadcast_changed` are shared.

- [ ] **Step 3: Tests pass, commit**

```bash
git add server/src/db.rs server/src/config.rs server/src/api.rs server/src/work.rs server/src/runner.rs
git commit -m "feat(server): a notice when a session's time is up, and a switch for it

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 14: `fx.ts` and `circle.ts`

**Files:**
- Create: `web/src/fx.ts` (port of `docs/superpowers/mockups/sundial/fx.js`), `web/src/circle.ts`, `web/src/circle.test.ts`

**Interfaces (`circle.ts`, pure):**
```ts
export const R = 148, C = 2 * Math.PI * R, SPAN = (C * 240) / 360
export type Pt = { x: number; y: number }
/** Bead i of total across the ring's opening (30°..150°, SVG degrees). */
export function beadAt(i: number, total: number): Pt
/** Arc segments for a task's steps: each { start (deg), len (px), frac } — done 1, current `frac`, rest 0. */
export function segments(steps: number, current: number, frac: number): { start: number; len: number; frac: number }[]
/** Where a horizontal release lands: 60 px, or 20 px when quick; clamped to [0, count-1]. */
export function slotAfter(index: number, dx: number, quick: boolean, count: number): number
/** The rail offset while dragging, rubber-banding at the ends by 0.3. */
export function railX(index: number, dx: number, count: number, width = 320): number
/** localStorage-backed counters for the two hints; `note.hints.start` and `note.hints.session`. */
export function hintShown(key: 'start' | 'session'): boolean   // count < 3
export function hintUsed(key: 'start' | 'session'): void        // count + 1
/** The session fields for a queue entry, as the Tasks view's startFocus builds them. */
export function sessionFor(entry: QueueEntry): SessionStart
export const WORK_TIME: SessionStart = { title: 'Work time' }
```
**Interfaces (`fx.ts`, DOM + gsap, every function returns early under reduced motion where the spec says):** `open(face, fill, track, done?)`, `closeRing(face, fill, beadIndex, total)`, `ripple(face, count?)`, `wave(face, color?, peak?)`, `bead(face, i, total)`, `beadsAtRest(face, done, total)`, `toBreak(face, fill, veilOn)`, `toWork(face, fill, track)`, `drain(fill, left)`. Signatures and numbers as in `fx.js` after the 2026-09-25 commits (`8719c44`). The `face` is the element holding `.arc svg`; the fill and track are the two `<circle>`s.

- [ ] **Step 1: Tests (`circle.test.ts`)**

```ts
import { expect, test } from 'vitest'
import { beadAt, segments, slotAfter, railX, SPAN, C } from './circle'

test('beads sit evenly across the opening', () => {
  const a = beadAt(0, 4), d = beadAt(3, 4)
  expect(a.x).toBeGreaterThan(160); expect(d.x).toBeLessThan(160)   // first on the right, last on the left
  expect(Math.hypot(a.x - 160, a.y - 160)).toBeCloseTo(148, 5)
  expect(a.y).toBeCloseTo(d.y, 5)
})
test('segments split the span with 5° gaps', () => {
  const s = segments(3, 1, 0.35)
  expect(s).toHaveLength(3)
  expect(s.map((x) => x.frac)).toEqual([1, 0.35, 0])
  expect(s[0].start).toBe(150)
  expect(s[1].start - s[0].start).toBeCloseTo((240 - 10) / 3 + 5, 5)
  expect(s.reduce((n, x) => n + x.len, 0)).toBeCloseTo((C * (240 - 10)) / 360, 3)
})
test('a release lands on the next slot at 60 px or a quick 20 px', () => {
  expect(slotAfter(1, -70, false, 4)).toBe(2)
  expect(slotAfter(1, -30, false, 4)).toBe(1)
  expect(slotAfter(1, -30, true, 4)).toBe(2)
  expect(slotAfter(0, 90, false, 4)).toBe(0)
  expect(slotAfter(3, -90, false, 4)).toBe(3)
})
test('the rail rubber-bands past the ends', () => {
  expect(railX(1, -40, 4)).toBe(-360)
  expect(railX(0, 100, 4)).toBe(30)
  expect(railX(3, -100, 4)).toBe(-990)
})
```
Add `sessionFor` tests like the earlier plan's (task with children and a step → `step_index` 2 of 3; Work time → `{ title: 'Work time' }`), and `hintShown`/`hintUsed` with a stubbed `localStorage`.

- [ ] **Step 2: Implement `circle.ts`**

```ts
export function beadAt(i: number, total: number): Pt {
  const a = ((30 + (120 * (i + 1)) / (total + 1)) * Math.PI) / 180
  return { x: 160 + R * Math.cos(a), y: 160 + R * Math.sin(a) }
}
export function segments(steps: number, current: number, frac: number) {
  const gap = 5, seg = (240 - gap * (steps - 1)) / steps
  return Array.from({ length: steps }, (_, i) => ({
    start: 150 + i * (seg + gap),
    len: (C * seg) / 360,
    frac: i < current ? 1 : i === current ? frac : 0,
  }))
}
export function slotAfter(index: number, dx: number, quick: boolean, count: number): number {
  const step = dx < -60 || (quick && dx < -20) ? 1 : dx > 60 || (quick && dx > 20) ? -1 : 0
  return Math.min(count - 1, Math.max(0, index + step))
}
export function railX(index: number, dx: number, count: number, width = 320): number {
  const edge = (index === 0 && dx > 0) || (index === count - 1 && dx < 0)
  return -index * width + (edge ? dx * 0.3 : dx)
}
```
Hints: read `note.hints.<key>` as a number in try/catch; `hintShown` is `< 3`.

- [ ] **Step 3: Port `fx.ts`**

Transcribe `fx.js` function by function, typed (`SVGCircleElement`, `HTMLElement`), with `still()` (the `prefers-reduced-motion` check from `motion-gsap.ts`) applied: `open` sets the track visible and the fill empty and calls `done` at once; `ripple`, `wave`, `bead`'s drop and the veil become immediate sets; `closeRing` sets the full dasharray and tweens only opacity for 0.2 s. Import `gsap` from 'gsap' (as `motion-gsap.ts` does), never a global. `wave` appends its overlay to the element passed as `frame` (a new second argument, the `.home` root) rather than `.frame-app`.

- [ ] **Step 4: Typecheck, test, commit**

```bash
git add web/src/fx.ts web/src/circle.ts web/src/circle.test.ts
git commit -m "feat(web): the arc's motion and geometry, ported from the mockups

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 15: The face: idle, tap, strip, gestures

**Files:**
- Modify: `web/src/views/Home.tsx` (the `session === null` branches of `bigFace`, `compactHeader`, `hero` at `Home.tsx:575-698`, `nextActions`), `web/src/gauge.tsx` (expose the fill and track circles via refs, or add a `Circle` variant that renders the two circles with ids the fx helpers can find), `web/src/api.ts` (`queue`, `rounds`, `endWorkSession` with `discard`), `web/src/types.ts` (`QueueEntry`, `QueueReason`), `web/src/app.tsx` (tab bar rest on mobile), `web/src/styles.css` (`.face-hint`, `.rail`, `.slot`, `.strip-dots`, `.handle`, `.tabs.away`, `--arc-sun`, `--arc-sage`)

**Interfaces:**
- Consumes: `circle.ts` and `fx.ts` from Task 14; `api.queue(limit)`, `api.rounds()`, `api.endWorkSession(id, outcome, discard?)`.
- Produces: Home state `phase: 'idle' | 'settling' | 'working' | 'paused' | 'break'`, `strip: { items: QueueEntry[]; index: number } | null`, `lastStartAt: number`.

- [ ] **Step 1: Types and api**

`types.ts`: `QueueReason`, `QueueEntry = { task: TaskNode; step: Task | null; planned_min: number | null; reason: QueueReason }`. `api.ts`: `queue: (limit = 5) => request<QueueEntry[]>(`/api/tasks/queue?limit=${limit}`)`, `rounds: () => request<{ rounds: number }>('/api/sessions/today')`, and `endWorkSession(id, outcome, discard = false)` sending `{ outcome, ...(discard && { discard: true }) }`.

- [ ] **Step 2: Idle**

When `session === null`: render the face box with no Gauge unless a block is coming (then the faded wait arc, title and start time as today, without the eyebrow word). Centre the hint `<p className="face-hint">tap the circle to start working</p>` when `hintShown('start')`, `pointer-events: none`. Remove the Start/Later/More action row from the idle face; keep the block's overflow reachable from the Today list below (it already is).

Mobile bar rest, in `app.tsx` where the `nav.tabs` renders: a `rested` state set true by a timer (4000 ms idle, 2500 ms while `session`), cleared by any `pointerdown` on the document; class `away` on the nav (`transform: translateY(100%); opacity: 0; transition: 500ms cubic-bezier(.4,0,.2,1)`), and a `.handle` (56×4, `--track`, bottom 8px, centred) shown while away. Desktop untouched.

- [ ] **Step 3: Tap → start → draw and unwind**

One pointer surface on the face box (`onPointerDown/Move/Up`, `touch-action: none`), the same state machine as `flow.html`:
- Tap (movement < 8 px) while idle: `hintUsed('start')`; `const items = await api.queue(5)`; if empty, toast "Nothing open to work on." and stay; else `openNow(sessionFor(items[0]))`, set `strip = { items, index: 1 }` (slot 0 is Work time), `lastStartAt = Date.now()`, `phase = 'settling'`, and run `fx.open(face, fill, track, () => setPhase('working'))`. The counter starts at the session's `started_at` regardless; the settling flag only blocks taps.
- Rail: slots `[Work time, ...items]`, each `<div class="slot">` with the hairline (`reason === 'overdue'` → rose; `urgent` or `due_soon` → sun), the counter (the existing `NowCounter`), the title (`gauge-name`), the step or notes line (`gauge-sub`). Neighbours at 35% opacity. Dots row 50 px under the arc, shown while dragging and for 1.4 s after.
- Sideways drag: `gsap.set(rail, { x: railX(index, dx, count) })`; on release `slotAfter(...)` → if it changed, `switchTo(slot)`: `discard = Date.now() - lastStartAt < 60_000`; `await api.endWorkSession(session.id, 'stopped', discard)`; `openNow(slot === 0 ? WORK_TIME : sessionFor(items[slot - 1]))`; `lastStartAt = Date.now()`. The arc keeps counting from the new session.
- Tap while working: pause/resume through the existing `pause`/`resume`; paused dims the fill to 38% and shows the glyph.
- Down ≥ 70 px: the existing `finish` (advance or complete), then Task 16's finish motion.
- Desktop: the face box is focusable; ArrowLeft/Right → `switchTo`, Escape → pause/resume, Enter → finish, `wheel` with `|deltaX| > |deltaY|` accumulates into a synthetic dx and snaps at 60.
- Second hint under the arc while `phase !== 'idle'` and `hintShown('session')`: "tap to pause · swipe down when done"; `hintUsed('session')` when a session starts.

- [ ] **Step 4: Verify and commit**

Harness shots at 390×844 and 1440×900: idle with the bar rested, mid-draw, working, mid-swipe with dots, paused. End any session you start.
```bash
git add web/src/views/Home.tsx web/src/gauge.tsx web/src/api.ts web/src/types.ts web/src/app.tsx web/src/styles.css
git commit -m "feat(web): the circle — tap to start, swipe to switch, tap to pause, swipe down to finish

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 16: Marks and transitions

**Files:**
- Modify: `web/src/views/Home.tsx` (session face), `web/src/session.ts` (nothing new unless a helper is missing), `web/src/styles.css` (`.veil`, `.pause-glyph`)

**Interfaces:**
- Consumes: `fx.closeRing/wave/toBreak/toWork/drain/beadsAtRest`, `circle.segments/beadAt`, `api.rounds()`.

- [ ] **Step 1: Beads**

`rounds` state from `api.rounds()` on session start and on `refresh`; `done = rounds + (session.mode === 'pomodoro' ? session.round - 1 : 0)`; `total = Math.max(4, done + 1)`; `fx.beadsAtRest(face, done, total)` whenever `done`/`total` change (remove old `.bead`s first). No beads when idle.

- [ ] **Step 2: Steps as segments**

When the session has `step_count`, the Gauge draws `segments(step_count, step_index - 1, phaseFrac)` instead of one track+fill: for each segment a track circle (`stroke-dasharray: len C`, `transform: rotate(start)`) and, for `frac > 0`, a fill circle with `len·frac`. Keep the single-arc drawing for sessions without steps. The line under the title is the step's title (already `stepOf`'s data), and the "n of m" text goes.

- [ ] **Step 3: Transitions**

- Local clock: when the counter reaches 0 in a pomodoro work phase, play round-done: `fx.closeRing(face, fill, done, total)`, `fx.wave(face, frame)` at 420 ms, then at 1300 ms `fx.toBreak(face, fill, true)`, fade the title/sub/hairline (0.5 s), and switch the counter to the break. Record `seenPhase = `${session.id}:break``. When the refetched session (after the server's `changed`) shows `phase === 'break'`, do nothing if `seenPhase` matches; if the server flipped first (tab was hidden), play the same sequence then.
- Break clock at 0 or server `phase === 'work'` with a new round: `fx.toWork(face, fill, track)`, title back (0.5 s from 0.4 s), then `fx.open(face, fill, track)` and the round counts from the server's `phase_started_at`.
- Break face: `fx.drain(fill, SPAN * remaining / phaseLength)` on every counter tick; the `.veil` (a `--ink` 14% overlay on the `.home` root) on while `phase === 'break'`.
- Finish (from Task 15's swipe-down or Done): `fx.closeRing` + `fx.wave`, then fade track/fill/beads (0.6 s) and clear to idle.
- `navigator.vibrate?.([30, 40, 30])` at round-done and at finish.

- [ ] **Step 4: Verify and commit**

Harness: set `pomodoro_enabled` on the audit copy, start from the circle, force a flip by `PATCH`ing nothing (wait for the sweep) or set `pomodoro_work_min` to 5 and use `Date` stubbing; shoot working with beads, break, and the return. Reset the setting; end the session.
```bash
git add web/src/views/Home.tsx web/src/styles.css
git commit -m "feat(web): rounds are beads, breaks drain the arc in sage, and a round's end runs into the next

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 17: The notify switch

**Files:**
- Modify: `web/src/types.ts` (`Settings.session_end_notify`), `web/src/api.ts` (`WRITABLE_SETTINGS`), `web/src/views/Settings.tsx` (a `Switch` row "Notify when a session ends" in the Sessions group beside Pomodoro)

- [ ] **Step 1: Wire it** the way `pomodoro_enabled` is wired (grep it in Settings.tsx and api.ts), label "Notify when a session ends", no explanatory sentence.
- [ ] **Step 2: Typecheck, test, harness shot of the row, commit**

```bash
git add web/src/types.ts web/src/api.ts web/src/views/Settings.tsx
git commit -m "feat(web): a switch for the end-of-session notice

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Audit harness note

An isolated copy of the real data runs at `http://127.0.0.1:3299`; the main checkout's vite on `http://localhost:5174` proxies to it. Login `shuntia` / `audit-pass`. `web/scripts/_audit-shot.mjs <tab> <WxH> <out.png>` takes a shot; clicks need `{ force: true }`. Worktrees start their own vite on another port with `NOTE_API=http://127.0.0.1:3299`. Server changes need `cargo build -p note-server` and a restart of the scratch server from its directory (`/tmp/claude-1000/-home-shuntia-Projects-note/20117f0c-d4d1-4d0a-932c-be964f2eba0c/scratchpad/audit`, binary `target/debug/note-server`). End every session you start.
