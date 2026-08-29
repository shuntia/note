# Note Agent Tools, Memory & Context Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The model-facing tool layer (serde+schemars registry with per-session-type surfaces, proptest fuzzing), the file-backed memory store with a derived SQLite lexical index, and pure-code context assembly — no LLM providers, no channels yet.

**Architecture:** Tools are plain Rust structs: the same type deserializes (and rejects) an incoming call and generates the JSON Schema handed to the LLM. A single `dispatch` entrypoint enforces the per-session registry, a payload cap, and per-call transactionality, returning typed `ToolError` values — never panics, never partial writes. Memory is one markdown file per fact under `data_dir/memory/<user>/{semantic,episodic,procedural,archive}/`; a SQLite FTS5 index is a derived cache, rebuilt at startup. Context assembly is pure code: standing.md (agent-edited in place) + a dynamic state block rendered from the DB.

**Tech Stack:** Existing foundation (axum 0.8, rusqlite bundled, jiff, uuid, anyhow, thiserror) plus `schemars` (schema generation) and the already-present `proptest`/`tempfile` dev-deps.

**Spec:** `docs/superpowers/specs/2026-08-29-note-design.md` (sections: Agent runtime, Memory layer, Context injection, Testing). Prior phase: `docs/superpowers/plans/2026-08-29-note-foundation.md`.

## Global Constraints

- All prior foundation constraints hold: wall-clock `HH:MM` strings + IANA tz names; SQLite pragmas `journal_mode=WAL`, `foreign_keys=ON` on every open; comments only where code can't speak, at fn declarations; `cargo test` green from repo root before every commit.
- **No filesystem or shell access is ever exposed to model-facing code.** Models supply content, never paths. Every string that reaches a filesystem join (memory ids, categories) is validated first: ids must be UUID-shaped, categories come from a closed serde enum.
- **Every tool call is transactional:** it fully applies or leaves no trace. DB work runs inside one transaction per dispatch. File work (memory, standing.md) happens file-first; the SQLite memory index is a derived cache that `memory::reindex_all` rebuilds at startup, so a crash between file write and index write self-heals.
- **Tool failures are typed rejections** (`ToolError { kind, message }`) returned as values — never a panic, never an `Err` that escapes dispatch as anything but `ToolError`.
- Decided semantics (these resolve open items from the foundation review; treat them as spec):
  - **Flexibility is a ladder:** `fixed` < `slide` < `drop`. A `drop` event is also slideable. The agent tool `schedule_drop` only accepts events with `flexibility = 'drop'`; the user-facing `POST /api/events/{id}/drop` route stays unrestricted (a human may drop anything).
  - **`slide_window_min` is a bound** on the *cumulative* offset from the event's original template time (new column `orig_wall_time`), enforced in `plan::shift` for windows > 0. `slide_window_min = 0` means unbounded.
  - **`snoozed` is owned:** `plan::snooze` sets `status = 'snoozed'` and pushes `wall_time` forward; the runner fires both `pending` and `snoozed` events when due. Snooze is allowed on `pending`, `snoozed`, and `fired` events (re-fire after "not now"), on any flexibility, and is not bounded by the slide window. Sliding a snoozed event returns it to `pending` (existing behavior, kept).

---

### Task 1: Transactional migrations + future-version guard

Precondition for authoring migration v2 (agreed in the foundation review): a partial migration failure on a live DB must not wedge startup, and a DB written by a newer binary must be refused, not silently half-migrated.

**Files:**
- Modify: `server/src/db.rs`

**Interfaces:**
- Consumes: existing `db::open`, `db::open_memory`, `MIGRATIONS`.
- Produces: `fn apply_migrations(conn: &Connection, migrations: &[&str]) -> anyhow::Result<()>` (private; `init` calls it). Behavior: each migration step + its `user_version` bump commit atomically; `user_version > migrations.len()` is an error.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `server/src/db.rs`:

```rust
    #[test]
    fn failed_migration_step_rolls_back_entirely() {
        let conn = Connection::open_in_memory().unwrap();
        // second statement fails; the first must not survive
        let bad: &[&str] = &["CREATE TABLE half (id INTEGER); CREATE TABLE half (id INTEGER);"];
        assert!(apply_migrations(&conn, bad).is_err());
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 0);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE name='half'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn newer_db_version_is_refused() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        let err = apply_migrations(&conn, MIGRATIONS).unwrap_err().to_string();
        assert!(err.contains("99"), "unexpected error: {err}");
    }
```

- [ ] **Step 2: Run to verify failure** — `cargo test --lib db` — expected: compile FAIL (`apply_migrations` not defined).

- [ ] **Step 3: Implement**

Replace `init` in `server/src/db.rs`:

```rust
fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    apply_migrations(conn, MIGRATIONS)
}

/// Each step and its `user_version` bump commit atomically, so a failure
/// partway through a step leaves the database exactly at the previous version.
fn apply_migrations(conn: &Connection, migrations: &[&str]) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version as usize > migrations.len() {
        anyhow::bail!(
            "database schema version {version} is newer than this binary supports ({})",
            migrations.len()
        );
    }
    for (i, sql) in migrations.iter().enumerate().skip(version as usize) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}
```

- [ ] **Step 4: Run** `cargo test` — expected: PASS (all existing tests too).

- [ ] **Step 5: Commit** — `git add -A && git commit -m "feat: transactional migrations with future-version guard"`

---

### Task 2: Schema v2 — orig_wall_time + memory index tables

**Files:**
- Modify: `server/src/db.rs` (append v2 to `MIGRATIONS`), `server/src/plan.rs` (`generate` populates `orig_wall_time`)

**Interfaces:**
- Produces: columns/tables later tasks rely on:
  - `events.orig_wall_time TEXT NOT NULL` — the event's time as generated from the template, before any slide; backfilled from `wall_time` for pre-v2 rows.
  - `memory_index (user TEXT, id TEXT, category TEXT, summary TEXT, archived INTEGER, path TEXT, PRIMARY KEY (user, id))`.
  - `memory_fts` — FTS5 virtual table `(user UNINDEXED, id UNINDEXED, summary, body)`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `server/src/db.rs`:

```rust
    #[test]
    fn v2_backfills_orig_wall_time_from_v1_rows() {
        let conn = Connection::open_in_memory().unwrap();
        apply_migrations(&conn, &MIGRATIONS[..1]).unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO plans (user_id, date, created_at) VALUES (1, '2026-08-31', 'x')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time) VALUES (1, 'nudge', '09:15')",
            [],
        )
        .unwrap();
        apply_migrations(&conn, MIGRATIONS).unwrap();
        let orig: String = conn
            .query_row("SELECT orig_wall_time FROM events WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(orig, "09:15");
    }

    #[test]
    fn memory_fts_is_available_and_searchable() {
        let conn = open_memory().unwrap();
        conn.execute(
            "INSERT INTO memory_fts (user, id, summary, body) VALUES ('aki', 'x', 'dentist appointment', 'call about the molar')",
            [],
        )
        .unwrap();
        let id: String = conn
            .query_row(
                "SELECT id FROM memory_fts WHERE memory_fts MATCH '\"molar\"' AND user = 'aki'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(id, "x");
    }
```

- [ ] **Step 2: Run to verify failure** — `cargo test --lib db` — expected: FAIL (no v2 migration; `memory_fts` missing).

- [ ] **Step 3: Implement**

Append to `MIGRATIONS` in `server/src/db.rs`:

```rust
    // v2
    "
    ALTER TABLE events ADD COLUMN orig_wall_time TEXT NOT NULL DEFAULT '';
    UPDATE events SET orig_wall_time = wall_time WHERE orig_wall_time = '';
    CREATE TABLE memory_index (
        user TEXT NOT NULL,
        id TEXT NOT NULL,
        category TEXT NOT NULL CHECK (category IN ('semantic','episodic','procedural')),
        summary TEXT NOT NULL,
        archived INTEGER NOT NULL DEFAULT 0,
        path TEXT NOT NULL,
        PRIMARY KEY (user, id)
    );
    CREATE VIRTUAL TABLE memory_fts USING fts5(user UNINDEXED, id UNINDEXED, summary, body);
    ",
```

In `server/src/plan.rs::generate`, include the new column in the event insert:

```rust
        tx.execute(
            "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, flexibility, slide_window_min, channel)
             VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6)",
            (plan_id, &ev.kind, &ev.time, &ev.flexibility, ev.slide_window_min, &ev.channel),
        )?;
```

(Note `?3` bound twice: `wall_time` and `orig_wall_time` start equal.)

- [ ] **Step 4: Run** `cargo test` — expected: PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat: schema v2 with orig_wall_time and memory index tables"`

---

### Task 3: Slide-window enforcement, snooze, runner + API updates

Implements the decided semantics from Global Constraints: cumulative slide bound, owned `snoozed` state, runner firing snoozed events.

**Files:**
- Modify: `server/src/plan.rs`, `server/src/runner.rs` (status filter), `server/src/api.rs` (snooze route)

**Interfaces:**
- Consumes: `events.orig_wall_time` (Task 2).
- Produces:
  - `plan::ShiftError { OutOfWindow { offset: i64, window: i64 }, Db(rusqlite::Error), Other(anyhow::Error) }` (thiserror).
  - `plan::shift(conn, user_id, event_id, minutes) -> Result<Option<()>, ShiftError>` — same behavior as before, plus: when the event's `slide_window_min > 0` and the resulting `|wall_time − orig_wall_time|` exceeds it, returns `Err(OutOfWindow)` and writes nothing.
  - `plan::snooze(conn, user_id, event_id, minutes: i64) -> anyhow::Result<Option<()>>` — `minutes` in `1..=1440` (else `Err`); event must be owned and status `pending`/`snoozed`/`fired` (else `Ok(None)`); sets `status='snoozed'`, `wall_time += minutes` clamped to `00:00..=23:59`.
  - `plan::event_flexibility(conn, user_id, event_id) -> anyhow::Result<Option<String>>` — owned-only lookup, for the tool layer's drop gating.
  - Route: `POST /api/events/{id}/snooze {minutes}` (200 / 404 / 400, same shape as shift).
  - Runner: fires events with status `pending` **or** `snoozed`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `server/src/plan.rs` (note `tmpl()` event 0 has `slide_window_min: 60`):

```rust
    #[test]
    fn shift_beyond_window_is_rejected_and_writes_nothing() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        generate(&conn, uid, &tmpl(), date).unwrap();
        let ev_id = events_for(&conn, uid, date).unwrap()[0].id;
        // window is ±60: +45 then +45 puts cumulative offset at 90
        assert!(shift(&conn, uid, ev_id, 45).unwrap().is_some());
        let err = shift(&conn, uid, ev_id, 45).unwrap_err();
        assert!(matches!(err, ShiftError::OutOfWindow { offset: 90, window: 60 }), "got {err:?}");
        assert_eq!(events_for(&conn, uid, date).unwrap()[0].wall_time, "09:45");
        // sliding back inside the window still works
        assert!(shift(&conn, uid, ev_id, -45).unwrap().is_some());
        assert_eq!(events_for(&conn, uid, date).unwrap()[0].wall_time, "09:00");
    }

    #[test]
    fn snooze_sets_status_and_pushes_time() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        generate(&conn, uid, &tmpl(), date).unwrap();
        let ev_id = events_for(&conn, uid, date).unwrap()[0].id;
        // snooze works on a fired event and is not window-bound
        conn.execute("UPDATE events SET status='fired' WHERE id=?1", [ev_id]).unwrap();
        assert!(snooze(&conn, uid, ev_id, 90).unwrap().is_some());
        let ev = &events_for(&conn, uid, date).unwrap()[0];
        assert_eq!(ev.status, "snoozed");
        assert_eq!(ev.wall_time, "10:30");
        // decided events cannot be snoozed
        conn.execute("UPDATE events SET status='done' WHERE id=?1", [ev_id]).unwrap();
        assert!(snooze(&conn, uid, ev_id, 10).unwrap().is_none());
        // range and ownership
        conn.execute("UPDATE events SET status='pending' WHERE id=?1", [ev_id]).unwrap();
        assert!(snooze(&conn, uid, ev_id, 0).is_err());
        let other = crate::auth::create_user(&conn, "b", "p", false).unwrap();
        assert!(snooze(&conn, other, ev_id, 10).unwrap().is_none());
    }

    #[test]
    fn event_flexibility_is_owner_scoped() {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "a", "p", false).unwrap();
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        generate(&conn, uid, &tmpl(), date).unwrap();
        let ev_id = events_for(&conn, uid, date).unwrap()[0].id;
        assert_eq!(event_flexibility(&conn, uid, ev_id).unwrap().as_deref(), Some("slide"));
        let other = crate::auth::create_user(&conn, "b", "p", false).unwrap();
        assert!(event_flexibility(&conn, other, ev_id).unwrap().is_none());
    }
```

Add to the `tests` module in `server/src/runner.rs` (reuse its existing `setup`/`one_event_template` helpers):

```rust
    #[test]
    fn snoozed_events_fire_when_due() {
        let (conn, tmp, uid) = setup("UTC");
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(&conn, uid, &one_event_template("09:00"), date).unwrap();
        conn.execute("UPDATE events SET status='snoozed', wall_time='09:30'", []).unwrap();
        let now: jiff::Timestamp = "2026-08-31T09:31:00Z".parse().unwrap();
        assert_eq!(fire_due(&conn, tmp.path(), now).unwrap().len(), 1);
    }
```

**Also update three existing tests in `plan.rs` that now collide with window enforcement** (they slide the window-60 template event far outside its window):

- `shift_clamps_to_the_day_and_clears_snoozed`: after `let mut t = tmpl();` add `t.events[0].slide_window_min = 0;` (unbounded, so `i64::MAX` clamping is still what's under test).
- `shift_does_not_resurrect_a_decided_event`: same one-line addition (cumulative +90 exceeds 60).
- `shift_moves_wall_time_and_respects_fixed`: no change needed (+45 is within ±60), but its second half calls `shift` expecting `Ok(None)` for fixed — that behavior is unchanged; leave it.

- [ ] **Step 2: Run to verify failure** — `cargo test --lib plan runner` — expected: compile FAIL (`ShiftError`, `snooze`, `event_flexibility` missing).

- [ ] **Step 3: Implement in `server/src/plan.rs`**

```rust
use thiserror::Error;

/// Distinguishes a slide that violates the event's window (a client/model
/// mistake) from infrastructure failures.
#[derive(Debug, Error)]
pub enum ShiftError {
    #[error("cumulative slide of {offset} min exceeds the ±{window} min window")]
    OutOfWindow { offset: i64, window: i64 },
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
```

Extend `owned_event` to also return `orig_wall_time` and `slide_window_min`:

```rust
fn owned_event(conn: &Connection, user_id: i64, event_id: i64) -> rusqlite::Result<Option<(String, String, String, i64)>> {
    conn.query_row(
        "SELECT e.wall_time, e.flexibility, e.orig_wall_time, e.slide_window_min
         FROM events e JOIN plans p ON p.id = e.plan_id
         WHERE e.id = ?1 AND p.user_id = ?2",
        (event_id, user_id),
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .optional()
}
```

Rework `shift` (keep its status-preservation CASE update exactly as-is):

```rust
fn parse_minutes(wall: &str) -> anyhow::Result<i64> {
    let (h, m) = wall.split_once(':').ok_or_else(|| anyhow::anyhow!("bad wall_time: {wall}"))?;
    Ok(h.parse::<i64>()? * 60 + m.parse::<i64>()?)
}

/// Moves the event's wall time by `minutes`, clamped inside the day and — when
/// the event carries a positive `slide_window_min` — bounded so the cumulative
/// offset from `orig_wall_time` stays within the window. Only a `snoozed`
/// event returns to `pending`; a decided or fired one keeps its status. `None`
/// when the event is not the user's or its flexibility is `fixed`.
pub fn shift(conn: &Connection, user_id: i64, event_id: i64, minutes: i64) -> Result<Option<()>, ShiftError> {
    let Some((wall, flex, orig, window)) = owned_event(conn, user_id, event_id)? else {
        return Ok(None);
    };
    if flex == "fixed" {
        return Ok(None);
    }
    let total = parse_minutes(&wall)?.saturating_add(minutes).clamp(0, 23 * 60 + 59);
    if window > 0 {
        let offset = total - parse_minutes(&orig)?;
        if offset.abs() > window {
            return Err(ShiftError::OutOfWindow { offset, window });
        }
    }
    conn.execute(
        "UPDATE events SET wall_time = ?1,
             status = CASE WHEN status = 'snoozed' THEN 'pending' ELSE status END
         WHERE id = ?2",
        (format!("{:02}:{:02}", total / 60, total % 60), event_id),
    )?;
    Ok(Some(()))
}

/// Postpones delivery: any owned, undecided event (pending/snoozed/fired) can
/// be snoozed regardless of flexibility, and the slide window does not apply —
/// snooze is "not now", not a schedule change.
pub fn snooze(conn: &Connection, user_id: i64, event_id: i64, minutes: i64) -> Result<Option<()>> {
    if !(1..=24 * 60).contains(&minutes) {
        anyhow::bail!("snooze minutes must be in 1..=1440, got {minutes}");
    }
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT e.wall_time, e.status FROM events e
             JOIN plans p ON p.id = e.plan_id
             WHERE e.id = ?1 AND p.user_id = ?2",
            (event_id, user_id),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((wall, status)) = row else { return Ok(None) };
    if status == "done" || status == "dropped" {
        return Ok(None);
    }
    let (h, m) = wall.split_once(':').ok_or_else(|| anyhow::anyhow!("bad wall_time: {wall}"))?;
    let total = (h.parse::<i64>()? * 60 + m.parse::<i64>()? + minutes).clamp(0, 23 * 60 + 59);
    conn.execute(
        "UPDATE events SET wall_time = ?1, status = 'snoozed' WHERE id = ?2",
        (format!("{:02}:{:02}", total / 60, total % 60), event_id),
    )?;
    Ok(Some(()))
}

pub fn event_flexibility(conn: &Connection, user_id: i64, event_id: i64) -> Result<Option<String>> {
    Ok(owned_event(conn, user_id, event_id)?.map(|(_, flex, _, _)| flex))
}
```

`set_status` calls `owned_event` — its destructuring changes to the 4-tuple (it only checks `is_none()`, so no other change).

In `server/src/runner.rs`, change the candidates query's status filter (line ~66) from `WHERE e.status = 'pending'` to `WHERE e.status IN ('pending','snoozed')` (keep any other conditions in that WHERE clause intact).

In `server/src/api.rs`, add the route `.route("/api/events/{id}/snooze", post(event_snooze))` and handler:

```rust
#[derive(Deserialize)]
struct SnoozeReq { minutes: i64 }

async fn event_snooze(
    user: CurrentUser, State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Json(req): Json<SnoozeReq>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::plan::snooze(&conn, user.id, id, req.minutes) {
        Ok(Some(())) => StatusCode::OK.into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}
```

The existing `event_shift` handler's `match` arms are unchanged (`Err(_) => BAD_REQUEST` covers `ShiftError` the same way).

- [ ] **Step 4: Run** `cargo test` — expected: PASS, including the three updated tests.

- [ ] **Step 5: Commit** — `git commit -am "feat: slide windows, snooze semantics, runner fires snoozed events"`

---

### Task 4: Memory store — files, index, query, reindex

**Files:**
- Create: `server/src/memory.rs`
- Modify: `server/src/lib.rs` (add `pub mod memory;`), `server/src/main.rs` (reindex at startup)
- Test: inline in `memory.rs`

**Interfaces:**
- Consumes: `memory_index`/`memory_fts` tables (Task 2), `db::open_memory` for tests.
- Produces (all `user` parameters are the server-controlled username, never model input):
  - `memory::CATEGORIES: [&str; 3]` = `["semantic", "episodic", "procedural"]`.
  - `memory::MemoryFile { id: String, category: String, summary: String, body: String, supersedes: Option<String>, created: String, archived: bool }`.
  - `memory::QueryHit { id: String, category: String, summary: String }` (Serialize).
  - `memory::WriteError { Archived(String), Io(std::io::Error), Db(rusqlite::Error), Other(anyhow::Error) }` (thiserror, all `#[from]` except `Archived`).
  - `memory::valid_id(&str) -> bool` — UUID shape; the only gate between model-supplied ids and the filesystem.
  - `memory::add(conn, data_dir, user, category, summary, body) -> anyhow::Result<String>` — returns new id.
  - `memory::read(data_dir, user, id) -> anyhow::Result<Option<MemoryFile>>`.
  - `memory::update(conn, data_dir, user, id, summary, body) -> Result<Option<()>, WriteError>` — in-place refinement; archived targets are `Err(Archived)`.
  - `memory::supersede(conn, data_dir, user, old_id, summary, body) -> Result<Option<String>, WriteError>` — writes replacement (same category, `supersedes:` set), moves old file to `archive/`, returns new id.
  - `memory::query(conn, user, q, limit) -> anyhow::Result<Vec<QueryHit>>` — lexical FTS5 over non-archived entries; arbitrary junk queries return `Ok(vec![])` or hits, never a syntax error.
  - `memory::reindex_user(conn, data_dir, user) -> anyhow::Result<()>`, `memory::reindex_all(conn, data_dir) -> anyhow::Result<()>`.

File format (one fact per file, `<id>.md`, knowit-style frontmatter):

```markdown
---
id: 3f2a…-uuid
category: semantic
summary: one line, no newlines
created: 2026-08-29T12:00:00Z
supersedes: <old-id>        (only on superseding facts)
---

body text
```

- [ ] **Step 1: Write the failing tests**

`server/src/memory.rs` `tests` module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> (rusqlite::Connection, tempfile::TempDir) {
        (crate::db::open_memory().unwrap(), tempfile::tempdir().unwrap())
    }

    #[test]
    fn add_read_roundtrip() {
        let (conn, tmp) = env();
        let id = add(&conn, tmp.path(), "aki", "semantic", "likes tea", "green tea, no sugar").unwrap();
        let f = read(tmp.path(), "aki", &id).unwrap().unwrap();
        assert_eq!(f.summary, "likes tea");
        assert_eq!(f.body, "green tea, no sugar");
        assert_eq!(f.category, "semantic");
        assert!(!f.archived);
        assert!(tmp.path().join("memory/aki/semantic").join(format!("{id}.md")).exists());
    }

    #[test]
    fn query_finds_by_body_token_and_excludes_archived() {
        let (conn, tmp) = env();
        let id = add(&conn, tmp.path(), "aki", "semantic", "dentist", "molar hurts on tuesdays").unwrap();
        let hits = query(&conn, "aki", "molar", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);

        let new_id = supersede(&conn, tmp.path(), "aki", &id, "dentist", "molar fixed").unwrap().unwrap();
        let hits = query(&conn, "aki", "molar", 10).unwrap();
        assert_eq!(hits.len(), 1, "old fact must leave the index");
        assert_eq!(hits[0].id, new_id);
    }

    #[test]
    fn supersede_archives_the_old_file_and_links_it() {
        let (conn, tmp) = env();
        let old = add(&conn, tmp.path(), "aki", "episodic", "s", "b").unwrap();
        let new = supersede(&conn, tmp.path(), "aki", &old, "s2", "b2").unwrap().unwrap();
        assert!(tmp.path().join("memory/aki/archive").join(format!("{old}.md")).exists());
        assert!(!tmp.path().join("memory/aki/episodic").join(format!("{old}.md")).exists());
        let f = read(tmp.path(), "aki", &new).unwrap().unwrap();
        assert_eq!(f.supersedes.as_deref(), Some(old.as_str()));
        // archived facts are readable but immutable
        let archived = read(tmp.path(), "aki", &old).unwrap().unwrap();
        assert!(archived.archived);
        assert!(matches!(
            update(&conn, tmp.path(), "aki", &old, "x", "y"),
            Err(WriteError::Archived(_))
        ));
        assert!(matches!(
            supersede(&conn, tmp.path(), "aki", &old, "x", "y"),
            Err(WriteError::Archived(_))
        ));
    }

    #[test]
    fn update_rewrites_in_place() {
        let (conn, tmp) = env();
        let id = add(&conn, tmp.path(), "aki", "procedural", "s", "b").unwrap();
        update(&conn, tmp.path(), "aki", &id, "s revised", "b revised").unwrap().unwrap();
        let f = read(tmp.path(), "aki", &id).unwrap().unwrap();
        assert_eq!(f.summary, "s revised");
        assert_eq!(query(&conn, "aki", "revised", 10).unwrap().len(), 1);
    }

    #[test]
    fn reindex_rebuilds_from_files_alone() {
        let (conn, tmp) = env();
        let id = add(&conn, tmp.path(), "aki", "semantic", "findme", "needle haystack").unwrap();
        conn.execute("DELETE FROM memory_index", []).unwrap();
        conn.execute("DELETE FROM memory_fts WHERE user='aki'", []).unwrap();
        assert!(query(&conn, "aki", "needle", 10).unwrap().is_empty());
        reindex_user(&conn, tmp.path(), "aki").unwrap();
        assert_eq!(query(&conn, "aki", "needle", 10).unwrap()[0].id, id);
    }

    #[test]
    fn ids_are_validated_before_touching_the_filesystem() {
        let (conn, tmp) = env();
        for bad in ["../../../etc/passwd", "..", "x/y", "", "A-UPPER-ID-000000000000000000000000000"] {
            assert!(!valid_id(bad), "{bad:?} must be invalid");
            assert!(read(tmp.path(), "aki", bad).unwrap().is_none());
            assert!(update(&conn, tmp.path(), "aki", bad, "s", "b").unwrap().is_none());
        }
    }

    #[test]
    fn hostile_query_strings_never_error() {
        let (conn, tmp) = env();
        add(&conn, tmp.path(), "aki", "semantic", "s", "b").unwrap();
        for q in ["\"unbalanced", "a OR OR", "(((", "*", "co-lu:mn NEAR/x", "", "   "] {
            query(&conn, "aki", q, 10).unwrap();
        }
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test --lib memory` — expected: compile FAIL.

- [ ] **Step 3: Implement `server/src/memory.rs`**

```rust
use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const CATEGORIES: [&str; 3] = ["semantic", "episodic", "procedural"];

#[derive(Debug, Clone)]
pub struct MemoryFile {
    pub id: String,
    pub category: String,
    pub summary: String,
    pub body: String,
    pub supersedes: Option<String>,
    pub created: String,
    pub archived: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct QueryHit {
    pub id: String,
    pub category: String,
    pub summary: String,
}

/// Distinguishes writes rejected by the supersede-never-delete rule from
/// infrastructure failures, so the tool layer can reject vs. 500 correctly.
#[derive(Debug, Error)]
pub enum WriteError {
    #[error("memory {0} is archived and immutable")]
    Archived(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Ids appear in file paths; anything but a lowercase-UUID shape is rejected
/// before any filesystem access.
pub fn valid_id(id: &str) -> bool {
    id.len() == 36 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f' | b'-'))
}

fn user_root(data_dir: &Path, user: &str) -> PathBuf {
    data_dir.join("memory").join(user)
}

fn render(f: &MemoryFile) -> String {
    let mut fm = format!(
        "---\nid: {}\ncategory: {}\nsummary: {}\ncreated: {}\n",
        f.id, f.category, f.summary, f.created
    );
    if let Some(s) = &f.supersedes {
        fm.push_str(&format!("supersedes: {s}\n"));
    }
    format!("{fm}---\n\n{}\n", f.body)
}

fn parse(raw: &str, archived: bool) -> Result<MemoryFile> {
    let rest = raw.strip_prefix("---\n").context("missing frontmatter")?;
    let (fm, body) = rest.split_once("\n---\n").context("unterminated frontmatter")?;
    let mut f = MemoryFile {
        id: String::new(),
        category: String::new(),
        summary: String::new(),
        body: body.trim().to_string(),
        supersedes: None,
        created: String::new(),
        archived,
    };
    for line in fm.lines() {
        let Some((k, v)) = line.split_once(": ") else { continue };
        match k {
            "id" => f.id = v.into(),
            "category" => f.category = v.into(),
            "summary" => f.summary = v.into(),
            "created" => f.created = v.into(),
            "supersedes" => f.supersedes = Some(v.into()),
            _ => {}
        }
    }
    if !valid_id(&f.id) {
        bail!("bad or missing id in memory file frontmatter");
    }
    if !CATEGORIES.contains(&f.category.as_str()) {
        bail!("bad category {:?} in memory file {}", f.category, f.id);
    }
    Ok(f)
}

fn locate(data_dir: &Path, user: &str, id: &str) -> Option<(PathBuf, bool)> {
    if !valid_id(id) {
        return None;
    }
    let name = format!("{id}.md");
    for cat in CATEGORIES {
        let p = user_root(data_dir, user).join(cat).join(&name);
        if p.exists() {
            return Some((p, false));
        }
    }
    let p = user_root(data_dir, user).join("archive").join(&name);
    p.exists().then_some((p, true))
}

fn index_insert(conn: &Connection, user: &str, f: &MemoryFile, path: &Path) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO memory_index (user, id, category, summary, archived, path)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        (user, &f.id, &f.category, &f.summary, f.archived as i64, path.to_string_lossy()),
    )?;
    conn.execute("DELETE FROM memory_fts WHERE user = ?1 AND id = ?2", (user, &f.id))?;
    if !f.archived {
        conn.execute(
            "INSERT INTO memory_fts (user, id, summary, body) VALUES (?1, ?2, ?3, ?4)",
            (user, &f.id, &f.summary, &f.body),
        )?;
    }
    Ok(())
}

fn one_line(summary: &str) -> String {
    summary.replace(['\n', '\r'], " ").trim().to_string()
}

pub fn add(conn: &Connection, data_dir: &Path, user: &str, category: &str, summary: &str, body: &str) -> Result<String> {
    if !CATEGORIES.contains(&category) {
        bail!("invalid category: {category}");
    }
    let f = MemoryFile {
        id: uuid::Uuid::new_v4().to_string(),
        category: category.into(),
        summary: one_line(summary),
        body: body.trim().into(),
        supersedes: None,
        created: jiff::Timestamp::now().to_string(),
        archived: false,
    };
    let dir = user_root(data_dir, user).join(category);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.md", f.id));
    std::fs::write(&path, render(&f))?;
    index_insert(conn, user, &f, &path)?;
    Ok(f.id)
}

pub fn read(data_dir: &Path, user: &str, id: &str) -> Result<Option<MemoryFile>> {
    let Some((path, archived)) = locate(data_dir, user, id) else {
        return Ok(None);
    };
    Ok(Some(parse(&std::fs::read_to_string(path)?, archived)?))
}

pub fn update(conn: &Connection, data_dir: &Path, user: &str, id: &str, summary: &str, body: &str) -> Result<Option<()>, WriteError> {
    let Some((path, archived)) = locate(data_dir, user, id) else {
        return Ok(None);
    };
    if archived {
        return Err(WriteError::Archived(id.into()));
    }
    let mut f = parse(&std::fs::read_to_string(&path)?, false)?;
    f.summary = one_line(summary);
    f.body = body.trim().into();
    std::fs::write(&path, render(&f))?;
    index_insert(conn, user, &f, &path)?;
    Ok(Some(()))
}

pub fn supersede(conn: &Connection, data_dir: &Path, user: &str, old_id: &str, summary: &str, body: &str) -> Result<Option<String>, WriteError> {
    let Some((old_path, archived)) = locate(data_dir, user, old_id) else {
        return Ok(None);
    };
    if archived {
        return Err(WriteError::Archived(old_id.into()));
    }
    let mut old = parse(&std::fs::read_to_string(&old_path)?, false)?;
    let new = MemoryFile {
        id: uuid::Uuid::new_v4().to_string(),
        category: old.category.clone(),
        summary: one_line(summary),
        body: body.trim().into(),
        supersedes: Some(old.id.clone()),
        created: jiff::Timestamp::now().to_string(),
        archived: false,
    };
    let new_path = user_root(data_dir, user).join(&new.category).join(format!("{}.md", new.id));
    std::fs::write(&new_path, render(&new))?;
    let arch_dir = user_root(data_dir, user).join("archive");
    std::fs::create_dir_all(&arch_dir)?;
    let arch_path = arch_dir.join(format!("{}.md", old.id));
    std::fs::rename(&old_path, &arch_path)?;
    old.archived = true;
    index_insert(conn, user, &old, &arch_path)?;
    index_insert(conn, user, &new, &new_path)?;
    Ok(Some(new.id))
}

/// Lexical search over non-archived facts. The raw query is reduced to quoted
/// alphanumeric tokens, so model- or user-supplied strings can never produce
/// an FTS5 syntax error.
pub fn query(conn: &Connection, user: &str, q: &str, limit: i64) -> Result<Vec<QueryHit>> {
    let tokens: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(16)
        .map(|t| format!("\"{t}\""))
        .collect();
    if tokens.is_empty() {
        return Ok(vec![]);
    }
    let mut stmt = conn.prepare(
        "SELECT f.id, i.category, i.summary
         FROM memory_fts f
         JOIN memory_index i ON i.user = f.user AND i.id = f.id
         WHERE memory_fts MATCH ?1 AND f.user = ?2 AND i.archived = 0
         ORDER BY rank LIMIT ?3",
    )?;
    let rows = stmt.query_map((tokens.join(" OR "), user, limit), |r| {
        Ok(QueryHit { id: r.get(0)?, category: r.get(1)?, summary: r.get(2)? })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Rebuilds one user's index rows from their files. Unparseable files are
/// logged and skipped so one corrupt fact cannot hide the rest.
pub fn reindex_user(conn: &Connection, data_dir: &Path, user: &str) -> Result<()> {
    conn.execute("DELETE FROM memory_index WHERE user = ?1", [user])?;
    conn.execute("DELETE FROM memory_fts WHERE user = ?1", [user])?;
    let dirs = CATEGORIES.iter().map(|c| (*c, false)).chain([("archive", true)]);
    for (dir_name, archived) in dirs {
        let dir = user_root(data_dir, user).join(dir_name);
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "md") {
                continue;
            }
            match std::fs::read_to_string(&path).map_err(anyhow::Error::from).and_then(|raw| parse(&raw, archived)) {
                Ok(f) => index_insert(conn, user, &f, &path)?,
                Err(e) => {
                    let _ = crate::log::record(conn, None, "memory_index_error",
                        &format!("{}: {e}", path.display()));
                }
            }
        }
    }
    Ok(())
}

pub fn reindex_all(conn: &Connection, data_dir: &Path) -> Result<()> {
    let root = data_dir.join("memory");
    let Ok(entries) = std::fs::read_dir(&root) else { return Ok(()) };
    for entry in entries.flatten() {
        if entry.path().is_dir() {
            reindex_user(conn, data_dir, &entry.file_name().to_string_lossy())?;
        }
    }
    Ok(())
}
```

Add `pub mod memory;` to `server/src/lib.rs`. In `server/src/main.rs`, after `let conn = db::open(...)`:

```rust
    memory::reindex_all(&conn, &cfg.data_dir)?;
```

(add `memory` to the existing `use note_server::{...}` import).

- [ ] **Step 4: Run** `cargo test` — expected: PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat: file-backed memory store with derived fts5 index"`

---

### Task 5: Context assembly + standing.md edits

**Files:**
- Create: `server/src/context.rs`
- Modify: `server/src/lib.rs` (add `pub mod context;`)
- Test: inline in `context.rs`

**Interfaces:**
- Consumes: `config::UserConfig::load`, `plan::events_for`, `event_log` table.
- Produces:
  - `context::standing_path(config_dir, user) -> PathBuf` = `config_dir/users/<user>/standing.md`.
  - `context::EditError { Missing, NoMatch, Ambiguous(usize), Io(std::io::Error) }` (thiserror).
  - `context::edit_replace(config_dir, user, find, replace) -> Result<(), EditError>` — `find` must match exactly once (empty `find` is `NoMatch`).
  - `context::edit_append(config_dir, user, text) -> Result<(), EditError>` — creates the file (and parent dirs) if missing; ensures a trailing newline.
  - `context::assemble(conn, config_dir, user_id, username, now: jiff::Timestamp) -> anyhow::Result<String>` — standing doc + dynamic block (local time, today's plan with statuses, last 10 event_log rows for the user). Pure code, no model call.

- [ ] **Step 1: Write the failing tests**

`server/src/context.rs` `tests` module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_dir() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let write = |rel: &str, c: &str| {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        };
        write("defaults/user.toml",
            "display_name = \"X\"\ntimezone = \"Asia/Tokyo\"\ntemplate = \"default\"\n");
        tmp
    }

    #[test]
    fn append_creates_then_replace_edits() {
        let tmp = cfg_dir();
        edit_append(tmp.path(), "aki", "- prefers morning calls").unwrap();
        edit_append(tmp.path(), "aki", "- studying for exams").unwrap();
        edit_replace(tmp.path(), "aki", "morning calls", "evening calls").unwrap();
        let text = std::fs::read_to_string(standing_path(tmp.path(), "aki")).unwrap();
        assert!(text.contains("evening calls"));
        assert!(text.contains("studying for exams"));
    }

    #[test]
    fn replace_rejects_missing_ambiguous_and_absent_file() {
        let tmp = cfg_dir();
        assert!(matches!(edit_replace(tmp.path(), "aki", "x", "y"), Err(EditError::Missing)));
        edit_append(tmp.path(), "aki", "dup dup").unwrap();
        assert!(matches!(edit_replace(tmp.path(), "aki", "nope", "y"), Err(EditError::NoMatch)));
        assert!(matches!(edit_replace(tmp.path(), "aki", "dup", "y"), Err(EditError::Ambiguous(2))));
        assert!(matches!(edit_replace(tmp.path(), "aki", "", "y"), Err(EditError::NoMatch)));
    }

    #[test]
    fn assemble_renders_all_sections_in_user_tz() {
        let tmp = cfg_dir();
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
        edit_append(tmp.path(), "aki", "remember: hates mornings").unwrap();
        let tmpl = crate::templates::Template {
            events: vec![crate::templates::TemplateEvent {
                kind: "checkin_call".into(), time: "09:00".into(),
                days: vec!["mon".into()], flexibility: "slide".into(),
                slide_window_min: 60, channel: "voice".into(),
            }],
        };
        // 2026-08-31 is a Monday; noon UTC = 21:00 JST same day
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(&conn, uid, &tmpl, date).unwrap();
        crate::log::record(&conn, Some(uid), "event_fired", "event 1").unwrap();
        let now: jiff::Timestamp = "2026-08-31T12:00:00Z".parse().unwrap();
        let out = assemble(&conn, tmp.path(), uid, "aki", now).unwrap();
        assert!(out.contains("hates mornings"), "{out}");
        assert!(out.contains("2026-08-31 21:00"), "{out}");
        assert!(out.contains("Asia/Tokyo"), "{out}");
        assert!(out.contains("09:00 checkin_call [pending] via voice"), "{out}");
        assert!(out.contains("event_fired"), "{out}");
    }

    #[test]
    fn assemble_without_standing_or_plan_still_works() {
        let tmp = cfg_dir();
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
        let now: jiff::Timestamp = "2026-08-31T12:00:00Z".parse().unwrap();
        let out = assemble(&conn, tmp.path(), uid, "aki", now).unwrap();
        assert!(out.contains("(no standing context yet)"), "{out}");
        assert!(out.contains("(no plan generated for today)"), "{out}");
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test --lib context` — expected: compile FAIL.

- [ ] **Step 3: Implement `server/src/context.rs`**

```rust
use anyhow::Result;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EditError {
    #[error("standing.md does not exist yet; use append")]
    Missing,
    #[error("find text not present in standing.md")]
    NoMatch,
    #[error("find text matches {0} places in standing.md; it must match exactly one")]
    Ambiguous(usize),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub fn standing_path(config_dir: &Path, user: &str) -> PathBuf {
    config_dir.join("users").join(user).join("standing.md")
}

pub fn edit_replace(config_dir: &Path, user: &str, find: &str, replace: &str) -> Result<(), EditError> {
    let path = standing_path(config_dir, user);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(EditError::Missing),
        Err(e) => return Err(e.into()),
    };
    if find.is_empty() {
        return Err(EditError::NoMatch);
    }
    match text.matches(find).count() {
        0 => Err(EditError::NoMatch),
        1 => {
            std::fs::write(&path, text.replacen(find, replace, 1))?;
            Ok(())
        }
        n => Err(EditError::Ambiguous(n)),
    }
}

pub fn edit_append(config_dir: &Path, user: &str, text: &str) -> Result<(), EditError> {
    let path = standing_path(config_dir, user);
    std::fs::create_dir_all(path.parent().expect("standing.md always has a parent"))?;
    let mut cur = std::fs::read_to_string(&path).unwrap_or_default();
    if !cur.is_empty() && !cur.ends_with('\n') {
        cur.push('\n');
    }
    cur.push_str(text);
    cur.push('\n');
    std::fs::write(&path, cur)?;
    Ok(())
}

/// Renders the full injection context: the standing document verbatim, then a
/// dynamic block from the DB. Ordered standing-first for prompt-cache
/// stability — the standing doc changes rarely, the dynamic block every call.
pub fn assemble(conn: &Connection, config_dir: &Path, user_id: i64, username: &str, now: jiff::Timestamp) -> Result<String> {
    let ucfg = crate::config::UserConfig::load(config_dir, username)?;
    let tz = jiff::tz::TimeZone::get(&ucfg.timezone).unwrap_or(jiff::tz::TimeZone::UTC);
    let local = now.to_zoned(tz);
    let standing = std::fs::read_to_string(standing_path(config_dir, username))
        .unwrap_or_else(|_| "(no standing context yet)".into());

    let mut out = String::new();
    out.push_str("# Standing context\n\n");
    out.push_str(standing.trim());
    out.push_str("\n\n# Now\n\n");
    out.push_str(&format!("{} ({})\n\n", local.strftime("%Y-%m-%d %H:%M"), ucfg.timezone));

    out.push_str("# Today's plan\n\n");
    let events = crate::plan::events_for(conn, user_id, local.date())?;
    if events.is_empty() {
        out.push_str("(no plan generated for today)\n");
    }
    for e in &events {
        out.push_str(&format!("- {} {} [{}] via {}\n", e.wall_time, e.kind, e.status, e.channel));
    }

    out.push_str("\n# Recent activity\n\n");
    let mut stmt = conn.prepare(
        "SELECT ts, kind, detail FROM event_log WHERE user_id = ?1 ORDER BY id DESC LIMIT 10",
    )?;
    let rows: Vec<(String, String, String)> = stmt
        .query_map([user_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if rows.is_empty() {
        out.push_str("(none)\n");
    }
    for (ts, kind, detail) in rows {
        out.push_str(&format!("- {ts} {kind}: {detail}\n"));
    }
    Ok(out)
}
```

Add `pub mod context;` to `server/src/lib.rs`.

- [ ] **Step 4: Run** `cargo test` — expected: PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat: context assembly and standing.md targeted edits"`

---

### Task 6: Tool framework + task tools

**Files:**
- Create: `server/src/tools/mod.rs`, `server/src/tools/task_ops.rs`
- Modify: `server/src/lib.rs` (add `pub mod tools;`), `server/Cargo.toml` (add `schemars = "1"` to `[dependencies]`)
- Test: inline in `tools/mod.rs`

**Interfaces:**
- Consumes: `tasks::create`, `tasks::update`, `tasks::UpdateError`, `tasks::TaskPatch`.
- Produces (the framework every later tool task plugs into):
  - `tools::SessionKind { Nightly, Checkin, Talk }` (Clone, Copy, Debug, PartialEq).
  - `tools::ToolError { kind: &'static str, message: String }` (Serialize) with constructors `rejected`, `invalid_args`, `not_found`, `forbidden`, `unknown_tool`, `internal` (each `fn (impl Into<String>) -> Self`).
  - `tools::ToolCtx<'a> { config_dir: &'a Path, data_dir: &'a Path, user_id: i64, username: &'a str }`.
  - `tools::MAX_ARGS_BYTES: usize = 64 * 1024`.
  - `tools::registry(kind) -> &'static [&'static str]`.
  - `tools::schemas(kind) -> Vec<serde_json::Value>` — one `{name, description, input_schema}` per registered tool.
  - `tools::dispatch(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, name: &str, raw_args: &str) -> Result<serde_json::Value, ToolError>` — payload cap, registry check, strict parse, one transaction around the handler.
  - `tools::task_ops::{CreateArgs, UpdateArgs}` and handlers `create`/`update` with signature `fn(conn: &Connection, ctx: &ToolCtx, args: T) -> Result<serde_json::Value, ToolError>`.
  - After this task the registries contain only `task_create`/`task_update`; Tasks 7–8 extend them to their final shapes.

- [ ] **Step 1: Add the dependency**

In `server/Cargo.toml` `[dependencies]`: `schemars = "1"`. If `cargo build` later shows `schemars::schema_for!` missing or a changed API under the resolved version, pin `schemars = "0.8"` and derive the same way — both expose `#[derive(JsonSchema)]` and `schema_for!`.

- [ ] **Step 2: Write the failing tests**

`tests` module in `server/src/tools/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> (rusqlite::Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        // argon2 is slow and irrelevant here; insert the user row directly
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        (conn, tempfile::tempdir().unwrap())
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir) -> ToolCtx<'a> {
        ToolCtx { config_dir: tmp.path(), data_dir: tmp.path(), user_id: 1, username: "aki" }
    }

    #[test]
    fn task_create_and_update_roundtrip() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"call dentist","description":"about the molar"}"#).unwrap();
        let id = out["task_id"].as_i64().unwrap();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{id},"state":"done"}}"#)).unwrap();
        assert_eq!(out["state"], "done");
    }

    #[test]
    fn unknown_tool_unknown_field_and_bad_json_are_typed() {
        let (conn, tmp) = env();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "rm_rf", "{}").unwrap_err();
        assert_eq!(e.kind, "unknown_tool");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"x","surprise":1}"#).unwrap_err();
        assert_eq!(e.kind, "invalid_args");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create", "not json").unwrap_err();
        assert_eq!(e.kind, "invalid_args");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            r#"{"task_id":999,"state":"done"}"#).unwrap_err();
        assert_eq!(e.kind, "not_found");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            r#"{"task_id":1,"state":"exploded"}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
    }

    #[test]
    fn oversized_payload_is_rejected_before_parsing() {
        let (conn, tmp) = env();
        let big = format!(r#"{{"title":"{}"}}"#, "x".repeat(MAX_ARGS_BYTES));
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create", &big).unwrap_err();
        assert_eq!(e.kind, "rejected");
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn schemas_cover_the_registry_and_are_objects() {
        for kind in [SessionKind::Nightly, SessionKind::Checkin, SessionKind::Talk] {
            let schemas = schemas(kind);
            assert_eq!(schemas.len(), registry(kind).len());
            for s in schemas {
                assert!(s["name"].is_string());
                assert!(!s["description"].as_str().unwrap().is_empty());
                assert!(s["input_schema"].is_object());
            }
        }
    }
}
```

- [ ] **Step 3: Run to verify failure** — `cargo test --lib tools` — expected: compile FAIL.

- [ ] **Step 4: Implement**

`server/src/tools/mod.rs`:

```rust
pub mod task_ops;

use rusqlite::Connection;
use serde::Serialize;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SessionKind {
    Nightly,
    Checkin,
    Talk,
}

/// A tool failure returned to the model as a value; `kind` is machine-matchable,
/// `message` is for the model to read.
#[derive(Debug, Serialize)]
pub struct ToolError {
    pub kind: &'static str,
    pub message: String,
}

impl ToolError {
    fn of(kind: &'static str) -> impl Fn(String) -> Self {
        move |message| Self { kind, message }
    }
    pub fn rejected(m: impl Into<String>) -> Self { Self::of("rejected")(m.into()) }
    pub fn invalid_args(m: impl Into<String>) -> Self { Self::of("invalid_args")(m.into()) }
    pub fn not_found(m: impl Into<String>) -> Self { Self::of("not_found")(m.into()) }
    pub fn forbidden(m: impl Into<String>) -> Self { Self::of("forbidden")(m.into()) }
    pub fn unknown_tool(m: impl Into<String>) -> Self { Self::of("unknown_tool")(m.into()) }
    pub fn internal(m: impl Into<String>) -> Self { Self::of("internal")(m.into()) }
}

pub struct ToolCtx<'a> {
    pub config_dir: &'a Path,
    pub data_dir: &'a Path,
    pub user_id: i64,
    pub username: &'a str,
}

pub const MAX_ARGS_BYTES: usize = 64 * 1024;

const CHECKIN: &[&str] = &["task_create", "task_update"];
const TALK: &[&str] = &["task_create", "task_update"];
const NIGHTLY: &[&str] = &["task_create", "task_update"];

pub fn registry(kind: SessionKind) -> &'static [&'static str] {
    match kind {
        SessionKind::Nightly => NIGHTLY,
        SessionKind::Checkin => CHECKIN,
        SessionKind::Talk => TALK,
    }
}

fn schema<T: schemars::JsonSchema>() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("schema serializes")
}

fn describe(name: &str) -> (&'static str, serde_json::Value) {
    match name {
        "task_create" => ("Create a new task for the current user.", schema::<task_ops::CreateArgs>()),
        "task_update" => ("Update a task's title, description, state, or notes.", schema::<task_ops::UpdateArgs>()),
        _ => unreachable!("describe covers every registered tool"),
    }
}

pub fn schemas(kind: SessionKind) -> Vec<serde_json::Value> {
    registry(kind)
        .iter()
        .map(|name| {
            let (description, input_schema) = describe(name);
            serde_json::json!({ "name": name, "description": description, "input_schema": input_schema })
        })
        .collect()
}

fn parse<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, ToolError> {
    serde_json::from_str(raw).map_err(|e| ToolError::invalid_args(e.to_string()))
}

/// Sole entrypoint for model-originated calls: enforces the payload cap and
/// the per-session registry, then runs the handler inside one transaction so
/// a failed call leaves no trace.
pub fn dispatch(conn: &Connection, ctx: &ToolCtx, kind: SessionKind, name: &str, raw_args: &str) -> Result<serde_json::Value, ToolError> {
    if raw_args.len() > MAX_ARGS_BYTES {
        return Err(ToolError::rejected(format!("arguments exceed {MAX_ARGS_BYTES} bytes")));
    }
    if !registry(kind).contains(&name) {
        return Err(if NIGHTLY.contains(&name) {
            ToolError::forbidden(format!("tool {name} is not available in this session type"))
        } else {
            ToolError::unknown_tool(format!("no such tool: {name}"))
        });
    }
    let tx = conn.unchecked_transaction().map_err(|e| ToolError::internal(e.to_string()))?;
    let out = run(&tx, ctx, name, raw_args)?;
    tx.commit().map_err(|e| ToolError::internal(e.to_string()))?;
    Ok(out)
}

fn run(conn: &Connection, ctx: &ToolCtx, name: &str, raw: &str) -> Result<serde_json::Value, ToolError> {
    match name {
        "task_create" => task_ops::create(conn, ctx, parse(raw)?),
        "task_update" => task_ops::update(conn, ctx, parse(raw)?),
        _ => unreachable!("registry guarantees a known name"),
    }
}
```

(The `forbidden`-vs-`unknown_tool` split checks membership in `NIGHTLY` because Nightly is defined as the superset registry — Task 8 keeps that property.)

`server/src/tools/task_ops.rs`:

```rust
use super::{ToolCtx, ToolError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateArgs {
    pub title: String,
    #[serde(default)]
    pub description: String,
}

pub fn create(conn: &Connection, ctx: &ToolCtx, args: CreateArgs) -> Result<serde_json::Value, ToolError> {
    let title = args.title.trim();
    if title.is_empty() || title.len() > 500 {
        return Err(ToolError::rejected("title must be 1..=500 characters"));
    }
    let task = crate::tasks::create(conn, ctx.user_id, title, "agent")
        .map_err(|e| ToolError::internal(e.to_string()))?;
    if !args.description.is_empty() {
        crate::tasks::update(conn, ctx.user_id, task.id, crate::tasks::TaskPatch {
            description: Some(args.description),
            ..Default::default()
        })
        .map_err(|e| ToolError::internal(e.to_string()))?;
    }
    Ok(serde_json::json!({ "task_id": task.id }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateArgs {
    pub task_id: i64,
    pub title: Option<String>,
    pub description: Option<String>,
    pub state: Option<String>,
    pub notes: Option<String>,
}

pub fn update(conn: &Connection, ctx: &ToolCtx, args: UpdateArgs) -> Result<serde_json::Value, ToolError> {
    let patch = crate::tasks::TaskPatch {
        title: args.title,
        description: args.description,
        state: args.state,
        notes: args.notes,
    };
    match crate::tasks::update(conn, ctx.user_id, args.task_id, patch) {
        Ok(Some(t)) => Ok(serde_json::json!({ "task_id": t.id, "state": t.state })),
        Ok(None) => Err(ToolError::not_found(format!("no task {}", args.task_id))),
        Err(crate::tasks::UpdateError::InvalidState(s)) => {
            Err(ToolError::rejected(format!("invalid state: {s}")))
        }
        Err(e) => Err(ToolError::internal(e.to_string())),
    }
}
```

Add `pub mod tools;` to `server/src/lib.rs`.

- [ ] **Step 5: Run** `cargo test` — expected: PASS.

- [ ] **Step 6: Commit** — `git commit -am "feat: tool dispatch framework with task tools"`

---

### Task 7: Memory + context tools

**Files:**
- Create: `server/src/tools/memory_ops.rs`, `server/src/tools/context_ops.rs`
- Modify: `server/src/tools/mod.rs` (module decls, registry entries, `describe`, `run` arms)
- Test: inline in the two new files

**Interfaces:**
- Consumes: `memory::{add, read, update, supersede, query, valid_id, WriteError}`, `context::{edit_replace, edit_append, EditError}`.
- Produces:
  - `memory_ops::QueryArgs { query: String, limit: i64 (default 8) }`, handler `query` → `{"results": [QueryHit…]}`.
  - `memory_ops::ReadArgs { id: String }`, handler `read` → `{id, category, summary, body, archived}`.
  - `memory_ops::WriteArgs { op: WriteOp, id: Option<String>, category: Option<Category>, summary: String, body: String }` with `WriteOp { Add, Update, Supersede }` and `Category { Semantic, Episodic, Procedural }` (both serde `rename_all = "snake_case"`), handler `write` → `{"id": …}`.
  - `context_ops::EditArgs { find: Option<String>, replace: Option<String>, append: Option<String> }`, handler `edit` → `{"ok": true}`.
  - Registry after this task: `CHECKIN` = task tools + `memory_query`, `memory_read`, `memory_write`; `TALK` and `NIGHTLY` = `CHECKIN` + `context_edit`.

- [ ] **Step 1: Write the failing tests**

`tests` module in `server/src/tools/memory_ops.rs`:

```rust
#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, SessionKind, ToolCtx};

    fn env() -> (rusqlite::Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        (conn, tempfile::tempdir().unwrap())
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir) -> ToolCtx<'a> {
        ToolCtx { config_dir: tmp.path(), data_dir: tmp.path(), user_id: 1, username: "aki" }
    }

    #[test]
    fn write_query_read_supersede_flow() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_write",
            r#"{"op":"add","category":"semantic","summary":"tea preference","body":"green, no sugar"}"#).unwrap();
        let id = out["id"].as_str().unwrap().to_string();

        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_query",
            r#"{"query":"tea"}"#).unwrap();
        assert_eq!(out["results"][0]["id"], id.as_str());

        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_read",
            &format!(r#"{{"id":"{id}"}}"#)).unwrap();
        assert_eq!(out["body"], "green, no sugar");

        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_write",
            &format!(r#"{{"op":"supersede","id":"{id}","summary":"tea preference","body":"switched to coffee"}}"#)).unwrap();
        let new_id = out["id"].as_str().unwrap();
        assert_ne!(new_id, id);

        // superseding an archived fact is a typed rejection
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_write",
            &format!(r#"{{"op":"supersede","id":"{id}","summary":"x","body":"y"}}"#)).unwrap_err();
        assert_eq!(e.kind, "rejected");
    }

    #[test]
    fn malformed_ids_categories_and_op_combos_are_typed() {
        let (conn, tmp) = env();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_read",
            r#"{"id":"../../../etc/passwd"}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_write",
            r#"{"op":"add","category":"../../evil","summary":"s","body":"b"}"#).unwrap_err();
        assert_eq!(e.kind, "invalid_args");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_write",
            r#"{"op":"add","summary":"s","body":"b"}"#).unwrap_err();
        assert_eq!(e.kind, "rejected"); // add without category
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_write",
            r#"{"op":"update","summary":"s","body":"b"}"#).unwrap_err();
        assert_eq!(e.kind, "rejected"); // update without id
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "memory_write",
            r#"{"op":"update","id":"00000000-0000-4000-8000-000000000000","summary":"s","body":"b"}"#).unwrap_err();
        assert_eq!(e.kind, "not_found");
    }
}
```

`tests` module in `server/src/tools/context_ops.rs`:

```rust
#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, SessionKind, ToolCtx};

    fn env() -> (rusqlite::Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        (conn, tempfile::tempdir().unwrap())
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir) -> ToolCtx<'a> {
        ToolCtx { config_dir: tmp.path(), data_dir: tmp.path(), user_id: 1, username: "aki" }
    }

    #[test]
    fn append_then_replace_edits_standing() {
        let (conn, tmp) = env();
        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "context_edit",
            r#"{"append":"- exam week"}"#).unwrap();
        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "context_edit",
            r#"{"find":"exam week","replace":"exams done"}"#).unwrap();
        let text = std::fs::read_to_string(
            crate::context::standing_path(tmp.path(), "aki")).unwrap();
        assert!(text.contains("exams done"));
    }

    #[test]
    fn bad_combinations_and_misses_are_typed() {
        let (conn, tmp) = env();
        for raw in [
            r#"{}"#,
            r#"{"find":"x"}"#,
            r#"{"append":"a","find":"x","replace":"y"}"#,
        ] {
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "context_edit", raw).unwrap_err();
            assert_eq!(e.kind, "rejected", "raw={raw}");
        }
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "context_edit",
            r#"{"find":"nope","replace":"y"}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
        // context_edit is not on the check-in surface
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "context_edit",
            r#"{"append":"x"}"#).unwrap_err();
        assert_eq!(e.kind, "forbidden");
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test --lib tools` — expected: compile FAIL.

- [ ] **Step 3: Implement**

`server/src/tools/memory_ops.rs`:

```rust
use super::{ToolCtx, ToolError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

const MAX_SUMMARY: usize = 200;
const MAX_BODY: usize = 16 * 1024;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryArgs {
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 { 8 }

pub fn query(conn: &Connection, ctx: &ToolCtx, args: QueryArgs) -> Result<serde_json::Value, ToolError> {
    if !(1..=50).contains(&args.limit) {
        return Err(ToolError::rejected("limit must be in 1..=50"));
    }
    let hits = crate::memory::query(conn, ctx.username, &args.query, args.limit)
        .map_err(|e| ToolError::internal(e.to_string()))?;
    Ok(serde_json::json!({ "results": hits }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadArgs {
    pub id: String,
}

pub fn read(conn: &Connection, ctx: &ToolCtx, args: ReadArgs) -> Result<serde_json::Value, ToolError> {
    let _ = conn;
    if !crate::memory::valid_id(&args.id) {
        return Err(ToolError::rejected("malformed memory id"));
    }
    match crate::memory::read(ctx.data_dir, ctx.username, &args.id) {
        Ok(Some(f)) => Ok(serde_json::json!({
            "id": f.id, "category": f.category, "summary": f.summary,
            "body": f.body, "archived": f.archived,
        })),
        Ok(None) => Err(ToolError::not_found(format!("no memory {}", args.id))),
        Err(e) => Err(ToolError::internal(e.to_string())),
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WriteOp { Add, Update, Supersede }

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Category { Semantic, Episodic, Procedural }

impl Category {
    fn as_str(&self) -> &'static str {
        match self {
            Category::Semantic => "semantic",
            Category::Episodic => "episodic",
            Category::Procedural => "procedural",
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteArgs {
    pub op: WriteOp,
    pub id: Option<String>,
    pub category: Option<Category>,
    pub summary: String,
    pub body: String,
}

pub fn write(conn: &Connection, ctx: &ToolCtx, args: WriteArgs) -> Result<serde_json::Value, ToolError> {
    if args.summary.trim().is_empty() || args.summary.len() > MAX_SUMMARY {
        return Err(ToolError::rejected(format!("summary must be 1..={MAX_SUMMARY} bytes")));
    }
    if args.body.len() > MAX_BODY {
        return Err(ToolError::rejected(format!("body must be at most {MAX_BODY} bytes")));
    }
    let need_id = || -> Result<String, ToolError> {
        let id = args.id.clone().ok_or_else(|| ToolError::rejected("this op requires an id"))?;
        if !crate::memory::valid_id(&id) {
            return Err(ToolError::rejected("malformed memory id"));
        }
        Ok(id)
    };
    match args.op {
        WriteOp::Add => {
            let cat = args.category.as_ref()
                .ok_or_else(|| ToolError::rejected("add requires a category"))?;
            crate::memory::add(conn, ctx.data_dir, ctx.username, cat.as_str(), &args.summary, &args.body)
                .map(|id| serde_json::json!({ "id": id }))
                .map_err(|e| ToolError::internal(e.to_string()))
        }
        WriteOp::Update => {
            let id = need_id()?;
            match crate::memory::update(conn, ctx.data_dir, ctx.username, &id, &args.summary, &args.body) {
                Ok(Some(())) => Ok(serde_json::json!({ "id": id })),
                Ok(None) => Err(ToolError::not_found(format!("no memory {id}"))),
                Err(crate::memory::WriteError::Archived(id)) => {
                    Err(ToolError::rejected(format!("memory {id} is archived and immutable")))
                }
                Err(e) => Err(ToolError::internal(e.to_string())),
            }
        }
        WriteOp::Supersede => {
            let id = need_id()?;
            match crate::memory::supersede(conn, ctx.data_dir, ctx.username, &id, &args.summary, &args.body) {
                Ok(Some(new_id)) => Ok(serde_json::json!({ "id": new_id })),
                Ok(None) => Err(ToolError::not_found(format!("no memory {id}"))),
                Err(crate::memory::WriteError::Archived(id)) => {
                    Err(ToolError::rejected(format!("memory {id} is archived and immutable")))
                }
                Err(e) => Err(ToolError::internal(e.to_string())),
            }
        }
    }
}
```

`server/src/tools/context_ops.rs`:

```rust
use super::{ToolCtx, ToolError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditArgs {
    pub find: Option<String>,
    pub replace: Option<String>,
    pub append: Option<String>,
}

pub fn edit(conn: &Connection, ctx: &ToolCtx, args: EditArgs) -> Result<serde_json::Value, ToolError> {
    let _ = conn;
    let result = match (args.find, args.replace, args.append) {
        (Some(find), Some(replace), None) => {
            crate::context::edit_replace(ctx.config_dir, ctx.username, &find, &replace)
        }
        (None, None, Some(text)) => crate::context::edit_append(ctx.config_dir, ctx.username, &text),
        _ => return Err(ToolError::rejected(
            "provide either find+replace or append, not a mix",
        )),
    };
    match result {
        Ok(()) => Ok(serde_json::json!({ "ok": true })),
        Err(crate::context::EditError::Io(e)) => Err(ToolError::internal(e.to_string())),
        Err(e) => Err(ToolError::rejected(e.to_string())),
    }
}
```

In `server/src/tools/mod.rs`: add `pub mod context_ops; pub mod memory_ops;`; update the registries —

```rust
const CHECKIN: &[&str] = &[
    "memory_query", "memory_read", "memory_write",
    "task_create", "task_update",
];
const TALK: &[&str] = &[
    "memory_query", "memory_read", "memory_write",
    "task_create", "task_update",
    "context_edit",
];
const NIGHTLY: &[&str] = &[
    "memory_query", "memory_read", "memory_write",
    "task_create", "task_update",
    "context_edit",
];
```

Extend `describe`:

```rust
        "memory_query" => ("Search the user's long-term memory; returns ids and summaries.", schema::<memory_ops::QueryArgs>()),
        "memory_read" => ("Read one memory in full by id.", schema::<memory_ops::ReadArgs>()),
        "memory_write" => ("Add, update, or supersede a memory. Superseding archives the old fact.", schema::<memory_ops::WriteArgs>()),
        "context_edit" => ("Edit the standing context document: replace a unique snippet or append a line.", schema::<context_ops::EditArgs>()),
```

Extend `run`:

```rust
        "memory_query" => memory_ops::query(conn, ctx, parse(raw)?),
        "memory_read" => memory_ops::read(conn, ctx, parse(raw)?),
        "memory_write" => memory_ops::write(conn, ctx, parse(raw)?),
        "context_edit" => context_ops::edit(conn, ctx, parse(raw)?),
```

- [ ] **Step 4: Run** `cargo test` — expected: PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat: memory and context tools"`

---

### Task 8: Schedule tools + final session registries

**Files:**
- Create: `server/src/tools/schedule_ops.rs`
- Modify: `server/src/tools/mod.rs` (module decl, final registries, `describe`, `run` arms), `server/src/templates.rs` (make `valid_time` `pub(crate)`)
- Test: inline in `schedule_ops.rs`

**Interfaces:**
- Consumes: `plan::{shift, snooze, set_status, event_flexibility, ShiftError}`, `templates::valid_time` (now `pub(crate) fn valid_time(s: &str) -> bool`).
- Produces:
  - `schedule_ops::SlideArgs { event_id: i64, minutes: i64 }` → handler `slide`.
  - `schedule_ops::SnoozeArgs { event_id: i64, minutes: i64 }` → handler `snooze`.
  - `schedule_ops::DropArgs { event_id: i64 }` → handler `drop_event` — **only `flexibility = 'drop'` events**; others get `rejected`.
  - `schedule_ops::InsertArgs { date: String, kind: String, time: String, flexibility: Flexibility, slide_window_min: i64 (default 0), channel: Channel }` with `Flexibility { Fixed, Slide, Drop }`, `Channel { Push, Voice }` (serde `rename_all = "snake_case"`) → handler `insert`; the plan for `date` must already exist (else `rejected` — plan generation is the nightly job's business, Phase 3).
  - Final registries: `CHECKIN` = memory + task tools + `schedule_slide`/`schedule_snooze`/`schedule_drop`; `TALK` = `CHECKIN` + `context_edit`; `NIGHTLY` = `TALK` + `schedule_insert`. Nightly stays the superset.

- [ ] **Step 1: Write the failing tests**

`tests` module in `server/src/tools/schedule_ops.rs`:

```rust
#[cfg(test)]
mod tests {
    use crate::tools::{dispatch, SessionKind, ToolCtx};

    fn env() -> (rusqlite::Connection, tempfile::TempDir) {
        let conn = crate::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        let tmpl = crate::templates::Template {
            events: vec![
                crate::templates::TemplateEvent {
                    kind: "checkin_call".into(), time: "09:00".into(),
                    days: vec!["mon".into()], flexibility: "slide".into(),
                    slide_window_min: 60, channel: "voice".into(),
                },
                crate::templates::TemplateEvent {
                    kind: "nudge".into(), time: "14:00".into(),
                    days: vec!["mon".into()], flexibility: "drop".into(),
                    slide_window_min: 0, channel: "push".into(),
                },
            ],
        };
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(&conn, 1, &tmpl, date).unwrap();
        (conn, tempfile::tempdir().unwrap())
    }

    fn ctx<'a>(tmp: &'a tempfile::TempDir) -> ToolCtx<'a> {
        ToolCtx { config_dir: tmp.path(), data_dir: tmp.path(), user_id: 1, username: "aki" }
    }

    #[test]
    fn slide_snooze_and_window_rejection() {
        let (conn, tmp) = env();
        dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_slide",
            r#"{"event_id":1,"minutes":30}"#).unwrap();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_slide",
            r#"{"event_id":1,"minutes":45}"#).unwrap_err();
        assert_eq!(e.kind, "rejected"); // cumulative 75 > window 60
        dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_snooze",
            r#"{"event_id":1,"minutes":15}"#).unwrap();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_snooze",
            r#"{"event_id":1,"minutes":0}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_slide",
            r#"{"event_id":99,"minutes":5}"#).unwrap_err();
        assert_eq!(e.kind, "not_found");
    }

    #[test]
    fn drop_only_applies_to_droppable_events() {
        let (conn, tmp) = env();
        // event 1 is flexibility=slide → agent cannot drop it
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_drop",
            r#"{"event_id":1}"#).unwrap_err();
        assert_eq!(e.kind, "rejected");
        // event 2 is flexibility=drop
        dispatch(&conn, &ctx(&tmp), SessionKind::Checkin, "schedule_drop",
            r#"{"event_id":2}"#).unwrap();
        let status: String = conn
            .query_row("SELECT status FROM events WHERE id=2", [], |r| r.get(0)).unwrap();
        assert_eq!(status, "dropped");
    }

    #[test]
    fn insert_is_nightly_only_and_validated() {
        let (conn, tmp) = env();
        let ok = r#"{"date":"2026-08-31","kind":"nudge","time":"16:30","flexibility":"drop","channel":"push"}"#;
        for kind in [SessionKind::Checkin, SessionKind::Talk] {
            let e = dispatch(&conn, &ctx(&tmp), kind, "schedule_insert", ok).unwrap_err();
            assert_eq!(e.kind, "forbidden");
        }
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "schedule_insert", ok).unwrap();
        assert!(out["event_id"].as_i64().unwrap() > 0);

        for (raw, why) in [
            (r#"{"date":"2026-08-31","kind":"nudge","time":"9:00","flexibility":"drop","channel":"push"}"#, "unpadded time"),
            (r#"{"date":"not-a-date","kind":"nudge","time":"09:00","flexibility":"drop","channel":"push"}"#, "bad date"),
            (r#"{"date":"2026-09-01","kind":"nudge","time":"09:00","flexibility":"drop","channel":"push"}"#, "no plan for date"),
            (r#"{"date":"2026-08-31","kind":"","time":"09:00","flexibility":"drop","channel":"push"}"#, "empty kind"),
            (r#"{"date":"2026-08-31","kind":"nudge","time":"09:00","flexibility":"drop","slide_window_min":-5,"channel":"push"}"#, "negative window"),
        ] {
            let e = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "schedule_insert", raw).unwrap_err();
            assert_eq!(e.kind, "rejected", "{why}");
        }
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Nightly, "schedule_insert",
            r#"{"date":"2026-08-31","kind":"nudge","time":"09:00","flexibility":"soft","channel":"push"}"#).unwrap_err();
        assert_eq!(e.kind, "invalid_args");
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test --lib tools` — expected: compile FAIL.

- [ ] **Step 3: Implement `server/src/tools/schedule_ops.rs`**

```rust
use super::{ToolCtx, ToolError};
use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SlideArgs {
    pub event_id: i64,
    pub minutes: i64,
}

pub fn slide(conn: &Connection, ctx: &ToolCtx, args: SlideArgs) -> Result<serde_json::Value, ToolError> {
    match crate::plan::shift(conn, ctx.user_id, args.event_id, args.minutes) {
        Ok(Some(())) => Ok(serde_json::json!({ "ok": true })),
        Ok(None) => Err(ToolError::not_found(format!(
            "no slideable event {} for this user", args.event_id
        ))),
        Err(e @ crate::plan::ShiftError::OutOfWindow { .. }) => Err(ToolError::rejected(e.to_string())),
        Err(e) => Err(ToolError::internal(e.to_string())),
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SnoozeArgs {
    pub event_id: i64,
    pub minutes: i64,
}

pub fn snooze(conn: &Connection, ctx: &ToolCtx, args: SnoozeArgs) -> Result<serde_json::Value, ToolError> {
    if !(1..=1440).contains(&args.minutes) {
        return Err(ToolError::rejected("snooze minutes must be in 1..=1440"));
    }
    match crate::plan::snooze(conn, ctx.user_id, args.event_id, args.minutes) {
        Ok(Some(())) => Ok(serde_json::json!({ "ok": true })),
        Ok(None) => Err(ToolError::not_found(format!(
            "no snoozable event {} for this user", args.event_id
        ))),
        Err(e) => Err(ToolError::internal(e.to_string())),
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DropArgs {
    pub event_id: i64,
}

pub fn drop_event(conn: &Connection, ctx: &ToolCtx, args: DropArgs) -> Result<serde_json::Value, ToolError> {
    match crate::plan::event_flexibility(conn, ctx.user_id, args.event_id) {
        Ok(None) => return Err(ToolError::not_found(format!("no event {}", args.event_id))),
        Ok(Some(flex)) if flex != "drop" => {
            return Err(ToolError::rejected(format!(
                "event {} has flexibility '{flex}'; only droppable events can be dropped by the agent",
                args.event_id
            )));
        }
        Ok(Some(_)) => {}
        Err(e) => return Err(ToolError::internal(e.to_string())),
    }
    match crate::plan::set_status(conn, ctx.user_id, args.event_id, "dropped") {
        Ok(Some(())) => Ok(serde_json::json!({ "ok": true })),
        Ok(None) => Err(ToolError::not_found(format!("no event {}", args.event_id))),
        Err(e) => Err(ToolError::internal(e.to_string())),
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Flexibility { Fixed, Slide, Drop }

impl Flexibility {
    fn as_str(&self) -> &'static str {
        match self {
            Flexibility::Fixed => "fixed",
            Flexibility::Slide => "slide",
            Flexibility::Drop => "drop",
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Channel { Push, Voice }

impl Channel {
    fn as_str(&self) -> &'static str {
        match self {
            Channel::Push => "push",
            Channel::Voice => "voice",
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InsertArgs {
    pub date: String,
    pub kind: String,
    pub time: String,
    pub flexibility: Flexibility,
    #[serde(default)]
    pub slide_window_min: i64,
    pub channel: Channel,
}

pub fn insert(conn: &Connection, ctx: &ToolCtx, args: InsertArgs) -> Result<serde_json::Value, ToolError> {
    let kind = args.kind.trim();
    if kind.is_empty() || kind.len() > 100 {
        return Err(ToolError::rejected("kind must be 1..=100 characters"));
    }
    if !crate::templates::valid_time(&args.time) {
        return Err(ToolError::rejected(format!("time must be zero-padded HH:MM, got {:?}", args.time)));
    }
    if !(0..=720).contains(&args.slide_window_min) {
        return Err(ToolError::rejected("slide_window_min must be in 0..=720"));
    }
    let date: jiff::civil::Date = args.date.parse()
        .map_err(|_| ToolError::rejected(format!("date must be YYYY-MM-DD, got {:?}", args.date)))?;
    let plan_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM plans WHERE user_id = ?1 AND date = ?2",
            (ctx.user_id, date.to_string()),
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| ToolError::internal(e.to_string()))?;
    let Some(plan_id) = plan_id else {
        return Err(ToolError::rejected(format!("no plan exists for {date}; generate it first")));
    };
    conn.execute(
        "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, flexibility, slide_window_min, channel)
         VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6)",
        (plan_id, kind, &args.time, args.flexibility.as_str(), args.slide_window_min, args.channel.as_str()),
    )
    .map_err(|e| ToolError::internal(e.to_string()))?;
    Ok(serde_json::json!({ "event_id": conn.last_insert_rowid() }))
}
```

(`use rusqlite::OptionalExtension;` is needed for `.optional()`.)

In `server/src/templates.rs`, change `fn valid_time` to `pub(crate) fn valid_time`.

In `server/src/tools/mod.rs`: add `pub mod schedule_ops;`; final registries —

```rust
const CHECKIN: &[&str] = &[
    "memory_query", "memory_read", "memory_write",
    "task_create", "task_update",
    "schedule_slide", "schedule_snooze", "schedule_drop",
];
const TALK: &[&str] = &[
    "memory_query", "memory_read", "memory_write",
    "task_create", "task_update",
    "schedule_slide", "schedule_snooze", "schedule_drop",
    "context_edit",
];
const NIGHTLY: &[&str] = &[
    "memory_query", "memory_read", "memory_write",
    "task_create", "task_update",
    "schedule_slide", "schedule_snooze", "schedule_drop",
    "context_edit", "schedule_insert",
];
```

Extend `describe`:

```rust
        "schedule_slide" => ("Slide a plan event by N minutes (negative = earlier), within its slide window.", schema::<schedule_ops::SlideArgs>()),
        "schedule_snooze" => ("Postpone a plan event's delivery by N minutes without changing the schedule intent.", schema::<schedule_ops::SnoozeArgs>()),
        "schedule_drop" => ("Drop a droppable plan event for today.", schema::<schedule_ops::DropArgs>()),
        "schedule_insert" => ("Insert a new event into an existing day plan.", schema::<schedule_ops::InsertArgs>()),
```

Extend `run`:

```rust
        "schedule_slide" => schedule_ops::slide(conn, ctx, parse(raw)?),
        "schedule_snooze" => schedule_ops::snooze(conn, ctx, parse(raw)?),
        "schedule_drop" => schedule_ops::drop_event(conn, ctx, parse(raw)?),
        "schedule_insert" => schedule_ops::insert(conn, ctx, parse(raw)?),
```

- [ ] **Step 4: Run** `cargo test` — expected: PASS (including Task 6's `schemas_cover_the_registry_and_are_objects`, which now walks the full registries).

- [ ] **Step 5: Commit** — `git commit -am "feat: schedule tools with droppable gating and final session registries"`

---

### Task 9: Proptest fuzzing + invariant harness

The spec's heaviest testing investment: the tool layer is the sole boundary between model output and system state.

**Files:**
- Create: `server/tests/tool_fuzz.rs`, `server/tests/tool_invariants.rs`
- Test: these files are the deliverable

**Interfaces:**
- Consumes: `tools::{dispatch, SessionKind, ToolCtx, MAX_ARGS_BYTES}`, `db::open_memory`, `plan::generate`, `templates::{Template, TemplateEvent}`, `memory::valid_id`.

- [ ] **Step 1: Write the fuzz test**

`server/tests/tool_fuzz.rs`:

```rust
use note_server::tools::{dispatch, SessionKind, ToolCtx, MAX_ARGS_BYTES};
use proptest::prelude::*;

fn setup() -> (rusqlite::Connection, tempfile::TempDir) {
    let conn = note_server::db::open_memory().unwrap();
    conn.execute(
        "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
        [],
    )
    .unwrap();
    let tmpl = note_server::templates::Template {
        events: vec![note_server::templates::TemplateEvent {
            kind: "nudge".into(), time: "09:00".into(),
            days: vec!["mon".into()], flexibility: "drop".into(),
            slide_window_min: 30, channel: "push".into(),
        }],
    };
    let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
    note_server::plan::generate(&conn, 1, &tmpl, date).unwrap();
    (conn, tempfile::tempdir().unwrap())
}

fn snapshot(conn: &rusqlite::Connection) -> Vec<i64> {
    ["users", "tasks", "plans", "events", "memory_index"]
        .iter()
        .map(|t| {
            conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0)).unwrap()
        })
        .collect()
}

fn event_state(conn: &rusqlite::Connection) -> Vec<(i64, String, String)> {
    let mut stmt = conn
        .prepare("SELECT id, wall_time, status FROM events ORDER BY id")
        .unwrap();
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap();
    rows.collect::<rusqlite::Result<_>>().unwrap()
}

fn memory_files(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let root = dir.join("memory");
    let Ok(walk) = std::fs::read_dir(&root) else { return out };
    for user in walk.flatten() {
        for cat in std::fs::read_dir(user.path()).into_iter().flatten().flatten() {
            for f in std::fs::read_dir(cat.path()).into_iter().flatten().flatten() {
                out.push(f.path().to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}

fn arb_name() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => prop_oneof![
            Just("task_create".to_string()), Just("task_update".to_string()),
            Just("memory_query".to_string()), Just("memory_read".to_string()),
            Just("memory_write".to_string()), Just("context_edit".to_string()),
            Just("schedule_slide".to_string()), Just("schedule_snooze".to_string()),
            Just("schedule_drop".to_string()), Just("schedule_insert".to_string()),
        ],
        1 => "[a-z_]{1,20}",
        1 => ".*",
    ]
}

fn arb_args() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("{}".to_string()),
        Just("[]".to_string()),
        Just("null".to_string()),
        Just("not json at all".to_string()),
        ".*",
        Just(r#"{"title": 42}"#.to_string()),
        Just(r#"{"title": null}"#.to_string()),
        Just(r#"{"event_id": 1, "minutes": 9223372036854775807}"#.to_string()),
        Just(r#"{"event_id": -1, "minutes": -9223372036854775808}"#.to_string()),
        Just(r#"{"op":"add","category":"semantic","summary":"s","body":"b","extra":1}"#.to_string()),
        Just(r#"{"op":"add","category":"../../../etc","summary":"s","body":"b"}"#.to_string()),
        Just(r#"{"op":"update","id":"../../../../etc/passwd","summary":"s","body":"b"}"#.to_string()),
        Just(r#"{"id":"..%2f..%2f..%2fetc%2fpasswd"}"#.to_string()),
        Just(r#"{"query":"'; DROP TABLE tasks; --"}"#.to_string()),
        Just(r#"{"find":"a","replace":"b","append":"c"}"#.to_string()),
        Just(r#"{"date":"2026-08-31","kind":"x","time":"25:99","flexibility":"drop","channel":"push"}"#.to_string()),
        Just(format!(r#"{{"title":"{}"}}"#, "x".repeat(MAX_ARGS_BYTES))),
        // valid calls mixed in so success paths are also exercised
        Just(r#"{"title":"a real task"}"#.to_string()),
        Just(r#"{"event_id":1,"minutes":10}"#.to_string()),
        Just(r#"{"op":"add","category":"semantic","summary":"s","body":"b"}"#.to_string()),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn dispatch_is_total_and_never_partially_writes(
        name in arb_name(),
        raw in arb_args(),
        kind_idx in 0..3usize,
    ) {
        let (conn, tmp) = setup();
        let ctx = ToolCtx {
            config_dir: tmp.path(), data_dir: tmp.path(),
            user_id: 1, username: "aki",
        };
        let kind = [SessionKind::Nightly, SessionKind::Checkin, SessionKind::Talk][kind_idx];
        let before_counts = snapshot(&conn);
        let before_events = event_state(&conn);
        let before_files = memory_files(tmp.path());

        let result = dispatch(&conn, &ctx, kind, &name, &raw);

        if result.is_err() {
            prop_assert_eq!(before_counts, snapshot(&conn), "failed call changed row counts");
            prop_assert_eq!(before_events, event_state(&conn), "failed call changed events");
            prop_assert_eq!(before_files, memory_files(tmp.path()), "failed call changed memory files");
        }
        // path-shaped ids/categories must never escape the memory root
        prop_assert!(!tmp.path().join("etc").exists());
        prop_assert!(!tmp.path().parent().unwrap().join("passwd").exists());
        // every event still carries a well-formed wall time
        for (_, wall, status) in event_state(&conn) {
            prop_assert!(wall.len() == 5 && wall.as_bytes()[2] == b':', "bad wall_time {wall}");
            prop_assert!(
                ["pending", "fired", "snoozed", "dropped", "done"].contains(&status.as_str())
            );
        }
    }
}
```

- [ ] **Step 2: Run** `cargo test --test tool_fuzz` — expected: PASS (if any case fails, that is a real tool-layer bug: fix the tool, not the test; proptest will print the minimal failing input).

- [ ] **Step 3: Write the invariant-sequence test**

`server/tests/tool_invariants.rs`:

```rust
use note_server::tools::{dispatch, SessionKind, ToolCtx};
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum Op {
    TaskCreate(String),
    TaskSetState(i64, String),
    MemAdd(String, String),
    MemUpdate(usize, String),
    MemSupersede(usize, String),
    MemQuery(String),
    Slide(i64, i64),
    Snooze(i64, i64),
    Drop(i64),
    CtxAppend(String),
    CtxReplace(String, String),
    Insert(String, String),
}

fn arb_op() -> impl Strategy<Value = Op> {
    let word = "[a-z]{1,12}";
    prop_oneof![
        word.prop_map(Op::TaskCreate),
        (1..6i64, prop_oneof![
            Just("open".to_string()), Just("done".to_string()),
            Just("dropped".to_string()), Just("bogus".to_string()),
        ]).prop_map(|(id, s)| Op::TaskSetState(id, s)),
        (word, word).prop_map(|(s, b)| Op::MemAdd(s, b)),
        (0..8usize, word).prop_map(|(i, s)| Op::MemUpdate(i, s)),
        (0..8usize, word).prop_map(|(i, s)| Op::MemSupersede(i, s)),
        word.prop_map(Op::MemQuery),
        (1..4i64, -200..200i64).prop_map(|(e, m)| Op::Slide(e, m)),
        (1..4i64, -10..100i64).prop_map(|(e, m)| Op::Snooze(e, m)),
        (1..4i64).prop_map(Op::Drop),
        word.prop_map(Op::CtxAppend),
        (word, word).prop_map(|(f, r)| Op::CtxReplace(f, r)),
        (prop_oneof![Just("2026-08-31".to_string()), Just("2026-09-01".to_string())], "([01][0-9]|2[0-3]):[0-5][0-9]")
            .prop_map(|(d, t)| Op::Insert(d, t)),
    ]
}

fn apply(conn: &rusqlite::Connection, ctx: &ToolCtx, op: &Op, mem_ids: &mut Vec<String>) {
    let pick = |ids: &Vec<String>, i: usize| -> String {
        if ids.is_empty() { "00000000-0000-4000-8000-000000000000".into() }
        else { ids[i % ids.len()].clone() }
    };
    let (name, raw) = match op {
        Op::TaskCreate(t) => ("task_create", format!(r#"{{"title":"{t}"}}"#)),
        Op::TaskSetState(id, s) => ("task_update", format!(r#"{{"task_id":{id},"state":"{s}"}}"#)),
        Op::MemAdd(s, b) => ("memory_write",
            format!(r#"{{"op":"add","category":"semantic","summary":"{s}","body":"{b}"}}"#)),
        Op::MemUpdate(i, s) => ("memory_write",
            format!(r#"{{"op":"update","id":"{}","summary":"{s}","body":"{s}"}}"#, pick(mem_ids, *i))),
        Op::MemSupersede(i, s) => ("memory_write",
            format!(r#"{{"op":"supersede","id":"{}","summary":"{s}","body":"{s}"}}"#, pick(mem_ids, *i))),
        Op::MemQuery(q) => ("memory_query", format!(r#"{{"query":"{q}"}}"#)),
        Op::Slide(e, m) => ("schedule_slide", format!(r#"{{"event_id":{e},"minutes":{m}}}"#)),
        Op::Snooze(e, m) => ("schedule_snooze", format!(r#"{{"event_id":{e},"minutes":{m}}}"#)),
        Op::Drop(e) => ("schedule_drop", format!(r#"{{"event_id":{e}}}"#)),
        Op::CtxAppend(t) => ("context_edit", format!(r#"{{"append":"{t}"}}"#)),
        Op::CtxReplace(f, r) => ("context_edit", format!(r#"{{"find":"{f}","replace":"{r}"}}"#)),
        Op::Insert(d, t) => ("schedule_insert",
            format!(r#"{{"date":"{d}","kind":"extra","time":"{t}","flexibility":"slide","slide_window_min":15,"channel":"push"}}"#)),
    };
    if let Ok(out) = dispatch(conn, ctx, SessionKind::Nightly, name, &raw) {
        if name == "memory_write" {
            if let Some(id) = out["id"].as_str() {
                mem_ids.push(id.to_string());
            }
        }
    }
}

fn assert_invariants(conn: &rusqlite::Connection, data_dir: &std::path::Path) {
    // 1. Every event's wall_time is well-formed and its status/flexibility valid.
    let mut stmt = conn
        .prepare("SELECT wall_time, orig_wall_time, status, flexibility, slide_window_min FROM events")
        .unwrap();
    let rows: Vec<(String, String, String, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let minutes = |w: &str| -> i64 {
        let (h, m) = w.split_once(':').unwrap();
        h.parse::<i64>().unwrap() * 60 + m.parse::<i64>().unwrap()
    };
    for (wall, orig, status, flex, window) in rows {
        assert!(wall.len() == 5 && minutes(&wall) < 24 * 60, "bad wall_time {wall}");
        assert!(["pending", "fired", "snoozed", "dropped", "done"].contains(&status.as_str()));
        assert!(["fixed", "slide", "drop"].contains(&flex.as_str()));
        // window bound: a never-snoozed event within a positive window stays inside it;
        // snoozed events are exempt by design, so only check non-snoozed ones.
        if window > 0 && status != "snoozed" {
            assert!(
                (minutes(&wall) - minutes(&orig)).abs() <= window,
                "event slid outside window: {orig} -> {wall} (window {window})"
            );
        }
    }

    // 2. Memory: index rows and files agree; ids unique; supersede chains resolve.
    let mut stmt = conn
        .prepare("SELECT id, archived, path FROM memory_index WHERE user='aki'")
        .unwrap();
    let index: Vec<(String, i64, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let mut seen = std::collections::HashSet::new();
    for (id, archived, path) in &index {
        assert!(note_server::memory::valid_id(id), "index holds bad id {id}");
        assert!(seen.insert(id.clone()), "duplicate memory id {id}");
        assert!(std::path::Path::new(path).exists(), "index points at missing file {path}");
        let in_archive = path.contains("/archive/");
        assert_eq!(in_archive, *archived == 1, "archived flag disagrees with location: {path}");
    }
    for (id, _, _) in &index {
        let f = note_server::memory::read(data_dir, "aki", id).unwrap().unwrap();
        if let Some(target) = f.supersedes {
            let t = note_server::memory::read(data_dir, "aki", &target).unwrap()
                .unwrap_or_else(|| panic!("supersede target {target} vanished"));
            assert!(t.archived, "superseded fact {target} was not archived");
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn interleaved_tool_calls_preserve_invariants(ops in proptest::collection::vec(arb_op(), 1..25)) {
        let conn = note_server::db::open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('aki', 'x', 'member')",
            [],
        )
        .unwrap();
        let tmpl = note_server::templates::Template {
            events: vec![
                note_server::templates::TemplateEvent {
                    kind: "checkin_call".into(), time: "09:00".into(),
                    days: vec!["mon".into()], flexibility: "slide".into(),
                    slide_window_min: 60, channel: "voice".into(),
                },
                note_server::templates::TemplateEvent {
                    kind: "nudge".into(), time: "14:00".into(),
                    days: vec!["mon".into()], flexibility: "drop".into(),
                    slide_window_min: 0, channel: "push".into(),
                },
                note_server::templates::TemplateEvent {
                    kind: "meds".into(), time: "20:00".into(),
                    days: vec!["mon".into()], flexibility: "fixed".into(),
                    slide_window_min: 0, channel: "push".into(),
                },
            ],
        };
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        note_server::plan::generate(&conn, 1, &tmpl, date).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            config_dir: tmp.path(), data_dir: tmp.path(),
            user_id: 1, username: "aki",
        };

        let mut mem_ids = Vec::new();
        for op in &ops {
            apply(&conn, &ctx, op, &mut mem_ids);
            assert_invariants(&conn, tmp.path());
        }
    }
}
```

- [ ] **Step 4: Run** `cargo test --test tool_invariants` — expected: PASS. If a case fails, apply superpowers:systematic-debugging to the shrunken input proptest reports: the bug is in the tool or store layer.

- [ ] **Step 5: Run the whole suite** — `cargo test` — expected: PASS.

- [ ] **Step 6: Commit** — `git commit -am "test: proptest fuzzing and invariant harness for the tool layer"`

---

### Task 10: README + full verification

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Document the new surface**

Append to `README.md` (after the existing config-layout section) a section covering, in this order, with this content adapted to the README's existing voice:

```markdown
## Memory & agent tools

Per-user long-term memory lives under `data/memory/<user>/{semantic,episodic,procedural,archive}/` —
one markdown fact per file, frontmatter with a one-line summary. Facts are
never deleted: superseding a fact writes a replacement and moves the old file
to `archive/`. A SQLite FTS index over these files is derived and rebuilt at
startup, so the files themselves are the backup-worthy source of truth.

The standing context document each agent session sees is
`config/users/<user>/standing.md`; agents edit it in place through the
`context_edit` tool, so its history is whatever your config dir's VCS says.

Model-facing capabilities are typed tool calls dispatched through a
per-session-type registry (check-in < talk < nightly). Every call is
validated, size-capped, and transactional; failures return typed rejections
to the model and never leave partial state.

Event scheduling semantics: `fixed` events cannot move; `slide` events can be
slid within ±`slide_window_min` minutes of their template time (0 = unbounded);
`drop` events can additionally be dropped by the agent. Snoozing is separate:
any undecided event can be snoozed ("not now"), which re-fires it later and is
not bounded by the slide window.
```

- [ ] **Step 2: Full verification** — `cargo test` from the repo root — expected: full PASS. Then `cargo build --release` — expected: clean build.

- [ ] **Step 3: Commit** — `git commit -am "docs: memory layout and tool-layer semantics in README"`

---

## Follow-on plans (not in this document)

3. Provider layer (Anthropic + OpenAI-compatible + embeddings; mocks) + agent runtime + nightly plan/debrief job. Vector retrieval joins the memory index here (embeddings provider + a vectors table beside `memory_index`).
4. Channels: web-push, Twilio voice bridge, WebSocket delivery; outreach tools (`send_nudge`, `place_call`) join the registries; auth-hardening pass (rate limiting, spawn_blocking argon2, Secure cookies, session GC/logout) lands with public ingress.
5. Web PWA.
