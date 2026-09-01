# Daylight Step 4a — The Now flag (server) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give a task a stored `is_now` flag with a server-enforced cap of three, so the Tasks UI's NOW / LATER / DONE TODAY groups survive a reload and an agent write can never overfill Now.

**Architecture:** `tasks.rs` stays the single owner of task invariants, as it already is for durations and the one-level hierarchy. The cap is an invariant of the store, not of a handler: every create and every update ends by re-establishing "at most three live tasks carry `is_now`", so no caller — HTTP, tool, or a future one — can leave the database over the cap. The two surfaces differ only in policy on an *explicit* over-cap promotion: the user is told Now is full (spec 4.1) and the agent is quietly trimmed (spec's step 4 acceptance box). A SQLite CHECK backs the "a step is never in Now" half so it cannot be violated even by raw SQL.

**Tech Stack:** Rust, axum, rusqlite (SQLite), serde/schemars, tokio test harness, proptest.

**Spec:** `docs/superpowers/plans/2026-09-01-daylight-ui-spec.md` — Step 4, items 4.1–4.3. The React rendering of the three groups is a separate plan; this one produces only the contract it codes against.

**Builds on:** `docs/superpowers/plans/2026-09-01-daylight-step-5a-task-model.md` (migration v6, `parent_id` hierarchy, `TaskNode`, `DurationActor`). This plan's migration is v7.

## Global Constraints

From the spec's "Global constraints", the ones that bind server work:

- Copy rules: sentence case; active voice; user vocabulary, never system vocabulary; no tool names in user-facing text. The one string here a user can see is the `error` body of a refused promotion, and the client's toast for it is fixed by spec 4.1: `Now is full — finish or move something first`.
- Every state-changing action gets feedback within 100 ms and, where destructive, an in-place Undo — so a write that displaces something must say what it displaced in its own response, not force a second round trip.
- Additive-only API: the shipped UI uses `GET/POST /api/tasks` and `PATCH /api/tasks/{id}`; new fields are optional and must not change the meaning of existing ones.
- Hard cap: Now holds at most 3 tasks. "Now never renders more than 3 tasks, including after agent writes (excess falls to Later, newest-demoted-first)."
- Undo restores a task to its exact prior group *and* state, so completing a task must not silently clear its flag.

---

## Design decisions (argued from the spec)

**Why `is_now` and not `now`.** `tasks.rs` already has a private `now()` returning a timestamp; `now` as a field would read as a time everywhere it appears. The schema already spells booleans `is_error` (v5) and `archived` (v2), so `is_now INTEGER NOT NULL DEFAULT 0` is in style, and `is_now: bool` on the wire is unambiguous for the client.

**Why the cap counts only live tasks.** Spec 4.2 requires undo to restore "previous state AND previous group", so completing a Now task cannot clear its flag — the flag is where the group lives. But a done task renders under DONE TODAY, not NOW, so if it still consumed a slot the header would read `NOW · 2 of 3` while refusing a third. The cap therefore counts a task as *in Now* only when `is_now = 1 AND parent_id IS NULL AND state IN ('open','in_progress')`. Done and dropped tasks keep the flag and cost nothing.

**Why the user is refused and the agent is trimmed.** These are not in tension; the spec states both. 4.1: "Attempting to move a 4th into Now shows toast `Now is full — finish or move something first` and does not move it" — that is the user, moving one task through the ⋯ menu, and a refusal is honest: the user chose the three that are there. The acceptance box: "Now never renders more than 3 tasks, including after agent writes (excess falls to Later, newest-demoted-first)" — that is the agent, which acts without a person watching and must not be able to fail silently into an over-full Now. So the policy keys off the same writer identity the duration provenance already uses.

**Why "newest" means the highest id.** Among the tasks already in Now, the newest is the one created last, which is the one at the bottom of a list ordered by id — the same row a person would drop first. Reading "newest" as "most recently promoted" would make an agent promotion demote the task it just promoted, i.e. a no-op. The task being written is always excluded from the demotion candidates, so a write always takes effect.

**Why the cap is re-established after every write, not just on promotion.** Reopening a done Now task (undo, spec 4.2) puts it back in the live set without anyone setting `is_now` — that path can overflow Now just as a promotion can. Running the same trim after every successful create and update covers it, is idempotent when the count is already at or under the cap, and leaves no path that can end over it.

**Why a step's flag is cleared rather than refused on reparenting.** Setting `is_now: true` on a step is a caller error and is refused (422). But `PATCH {"parent_id": n}` on a task that happens to be in Now is not about Now at all, and failing it would make `task_split`'s and the UI's reparenting fail for an unrelated reason. Becoming a step *is* leaving Now, so the flag clears with the move.

---

## File Structure

- `server/src/db.rs` — migration v7 (the column, its CHECKs, an index) plus its migration test. The `MIGRATIONS` array is the established pattern.
- `server/src/tasks.rs` — the model: `is_now` on `Task`/`NewTask`/`TaskPatch`, `NOW_CAP`, the live-Now query, the trim, and the refusal. `DurationActor` becomes `Actor` because it now decides duration provenance *and* cap policy — one writer identity, one type.
- `server/src/api.rs` — the `Actor` rename at three call sites, a `Conflict` arm in `task_error`.
- `server/src/tools/task_ops.rs` — `is_now` on `CreateArgs`/`UpdateArgs`, demoted ids in the tool result.
- `server/src/tools/mod.rs` — tool descriptions that state the cap, and the unit tests for setting and clearing the flag.
- `server/tests/tasks_api.rs` — HTTP round-trip, refusal, demotion, and step tests.
- `server/tests/tool_invariants.rs` — a promote/demote operation and the standing invariant that no user ever holds more than three live Now tasks and no step is flagged.

---

### Task 1: Schema — v7 migration

**Files:**
- Modify: `server/src/db.rs` (the `MIGRATIONS` array and its `mod tests` block)

**Interfaces:**
- Produces: `tasks.is_now INTEGER NOT NULL DEFAULT 0`, constrained to 0/1 and to `parent_id IS NULL` when set; index `idx_tasks_now`.

- [ ] **Step 1: Write the failing test**

In `server/src/db.rs`, inside `mod tests`:

```rust
    #[test]
    fn v7_adds_is_now_and_keeps_it_off_steps() {
        let conn = open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (user_id, title, created_at, updated_at)
             VALUES (1, 'parent', 'now', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (user_id, title, parent_id, created_at, updated_at)
             VALUES (1, 'step', 1, 'now', 'now')",
            [],
        )
        .unwrap();
        let flag: i64 = conn
            .query_row("SELECT is_now FROM tasks WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(flag, 0);
        conn.execute("UPDATE tasks SET is_now = 1 WHERE id = 1", []).unwrap();
        assert!(conn.execute("UPDATE tasks SET is_now = 1 WHERE id = 2", []).is_err());
        assert!(conn.execute("UPDATE tasks SET is_now = 2 WHERE id = 1", []).is_err());
        assert!(conn.execute("UPDATE tasks SET parent_id = 2 WHERE id = 1", []).is_err());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p note-server --lib db::tests::v7`
Expected: FAIL — `no such column: is_now`.

- [ ] **Step 3: Write minimal implementation**

Append to `MIGRATIONS` in `server/src/db.rs`, after the v6 string:

```rust
    // v7
    "
    ALTER TABLE tasks ADD COLUMN is_now INTEGER NOT NULL DEFAULT 0
        CHECK (is_now IN (0, 1) AND (is_now = 0 OR parent_id IS NULL));
    CREATE INDEX idx_tasks_now ON tasks(user_id, is_now);
    ",
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p note-server --lib db::`
Expected: PASS, including `migrations_apply_and_are_idempotent` (which asserts `user_version == MIGRATIONS.len()`).

- [ ] **Step 5: Commit**

```bash
git add server/src/db.rs
git commit -m "feat(db): a task can be flagged into Now"
```

---

### Task 2: Model — the flag, the cap, and the two policies

**Files:**
- Modify: `server/src/tasks.rs`
- Modify: `server/src/api.rs` (the `Actor` rename and a `Conflict` arm)
- Modify: `server/src/tools/task_ops.rs` (the `Actor` rename only)
- Test: `server/tests/tasks_api.rs`

**Interfaces:**
- Produces:
  - `pub const NOW_CAP: usize = 3;`
  - `pub enum Actor { User, Agent }` (`Default` = `User`) — replaces `DurationActor`; the variants keep their names.
  - `Task` gains `pub is_now: bool` (last field, after `parent_id`).
  - `NewTask` gains `#[serde(default)] pub is_now: bool`.
  - `TaskPatch` gains `pub is_now: Option<bool>` and renames `duration_actor` to `actor: Actor` (still `#[serde(skip)]`).
  - `Updated` gains `pub demoted_from_now: Vec<i64>` (`#[serde(skip_serializing_if = "Vec::is_empty")]`).
  - `UpdateError::NowFull(String)`.
  - `pub fn create(conn, user_id, new: NewTask, source: &str, actor: Actor) -> Result<Task, UpdateError>` — same shape, renamed parameter type.
- Consumes: Task 1's column.

- [ ] **Step 1: Write the failing tests**

Add to `server/tests/tasks_api.rs` (its `app_with_user`, `post`, `patch_task`, and `list` helpers already exist at the top of the file):

```rust
#[tokio::test]
async fn is_now_round_trips_through_create_patch_and_list() {
    let (app, cookie, _tmp) = app_with_user().await;
    let (status, t) = post(&app, &cookie, "/api/tasks", r#"{"title":"call dentist"}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(t["is_now"], false, "a new task lands in Later");

    let (status, t) = patch_task(&app, &cookie, 1, r#"{"is_now":true}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(t["is_now"], true);
    assert_eq!(list(&app, &cookie).await[0]["is_now"], true, "the flag survives a reload");

    let (_, t) = patch_task(&app, &cookie, 1, r#"{"is_now":false}"#).await;
    assert_eq!(t["is_now"], false);

    // an unrelated patch leaves the flag alone
    patch_task(&app, &cookie, 1, r#"{"is_now":true}"#).await;
    let (_, t) = patch_task(&app, &cookie, 1, r#"{"notes":"ring at 9"}"#).await;
    assert_eq!(t["is_now"], true);

    // and a task can be born in Now
    let (_, t) = post(&app, &cookie, "/api/tasks", r#"{"title":"refill meds","is_now":true}"#).await;
    assert_eq!(t["is_now"], true);
}

#[tokio::test]
async fn a_fourth_task_is_refused_entry_to_now() {
    let (app, cookie, _tmp) = app_with_user().await;
    for title in ["a", "b", "c", "d"] {
        post(&app, &cookie, "/api/tasks", &format!(r#"{{"title":"{title}"}}"#)).await;
    }
    for id in 1..=3 {
        let (status, _) = patch_task(&app, &cookie, id, r#"{"is_now":true}"#).await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, body) = patch_task(&app, &cookie, 4, r#"{"is_now":true}"#).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("Now"));
    let v = list(&app, &cookie).await;
    assert_eq!(v[3]["is_now"], false, "the refused task did not move");
    assert_eq!(v[0]["is_now"], true, "and nothing already in Now was displaced");

    // creating a fourth straight into Now is refused the same way
    let (status, _) = post(&app, &cookie, "/api/tasks", r#"{"title":"e","is_now":true}"#).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // re-asserting the flag on a task already in Now is not a fourth
    let (status, _) = patch_task(&app, &cookie, 1, r#"{"is_now":true}"#).await;
    assert_eq!(status, StatusCode::OK);

    // finishing one frees its slot without clearing its flag
    patch_task(&app, &cookie, 1, r#"{"state":"done"}"#).await;
    let (status, t) = patch_task(&app, &cookie, 4, r#"{"is_now":true}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(t["is_now"], true);
    let v = list(&app, &cookie).await;
    assert_eq!(v[0]["is_now"], true, "undo must find the done task still in Now");
    assert_eq!(v[0]["state"], "done");
}

#[tokio::test]
async fn reopening_a_done_now_task_demotes_the_newest_instead_of_failing() {
    let (app, cookie, _tmp) = app_with_user().await;
    for title in ["a", "b", "c", "d"] {
        post(&app, &cookie, "/api/tasks", &format!(r#"{{"title":"{title}","is_now":true}}"#)).await;
        // the fourth would be refused, so free a slot first by finishing the first
        if title == "c" {
            patch_task(&app, &cookie, 1, r#"{"state":"done"}"#).await;
        }
    }
    // Now holds b, c, d live; a is done but still flagged
    let (status, t) = patch_task(&app, &cookie, 1, r#"{"state":"open"}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(t["is_now"], true, "undo restores the task to its old group");
    assert_eq!(t["demoted_from_now"][0], 4, "the newest fell back to Later");
    let v = list(&app, &cookie).await;
    let live_now = v
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["is_now"] == true && t["state"] != "done")
        .count();
    assert_eq!(live_now, 3);
    assert_eq!(v[3]["is_now"], false);
}

#[tokio::test]
async fn a_step_can_never_be_in_now() {
    let (app, cookie, _tmp) = app_with_user().await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"email landlord"}"#).await;
    let (status, step) =
        post(&app, &cookie, "/api/tasks", r#"{"title":"find the thread","parent_id":1}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(step["is_now"], false);

    let (status, _) = patch_task(&app, &cookie, 2, r#"{"is_now":true}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = post(
        &app,
        &cookie,
        "/api/tasks",
        r#"{"title":"another step","parent_id":1,"is_now":true}"#,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // a task in Now that becomes a step leaves Now with the move
    post(&app, &cookie, "/api/tasks", r#"{"title":"loose","is_now":true}"#).await;
    let (status, t) = patch_task(&app, &cookie, 3, r#"{"parent_id":1}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(t["is_now"], false);
    assert_eq!(t["parent_id"], 1);

    // and steps carry the field so the client never has to guess
    let v = list(&app, &cookie).await;
    assert_eq!(v[0]["children"][0]["is_now"], false);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p note-server --test tasks_api`
Expected: FAIL — `is_now` is an unknown field on the patch and absent from every response.

- [ ] **Step 3: Rename `DurationActor` to `Actor`**

In `server/src/tasks.rs`, rename the type and the `TaskPatch` field:

```rust
/// The writer's identity. It fixes a duration's provenance, and it decides what
/// an over-full Now does: a person is told, an agent is trimmed.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum Actor {
    #[default]
    User,
    Agent,
}

impl Actor {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
        }
    }
}
```

Then, mechanically across `server/src/tasks.rs`, `server/src/api.rs`, and `server/src/tools/task_ops.rs`:
- `DurationActor` → `Actor` (the `use` in `task_ops.rs` too)
- `patch.duration_actor` → `patch.actor`, and the `TaskPatch` field declaration to `#[serde(skip)] pub actor: Actor`
- in `task_ops::update`, the struct literal line `duration_actor: DurationActor::Agent,` → `actor: Actor::Agent,`

- [ ] **Step 4: Add the flag and the cap to `server/src/tasks.rs`**

Add the constant beside the other limits at the top of the file:

```rust
pub const NOW_CAP: usize = 3;
```

Add the error variant to `UpdateError`:

```rust
    #[error("{0}")]
    NowFull(String),
```

Add `is_now` to the three shapes and to the row mapping. `Task`:

```rust
    pub parent_id: Option<i64>,
    pub is_now: bool,
}
```

`NewTask`:

```rust
    #[serde(default)]
    pub is_now: bool,
}
```

`TaskPatch`:

```rust
    pub is_now: Option<bool>,
```

`row_to_task` and `COLS`:

```rust
        parent_id: r.get(8)?,
        is_now: r.get(9)?,
    })
}

const COLS: &str = "id, title, description, state, source, notes, duration_min, \
                    duration_source, parent_id, is_now";
```

Then the cap itself, placed after `checked_parent`:

```rust
/// Ids of the tasks that render in Now, oldest first. A done or dropped task
/// keeps its flag — that is where undo finds its group again — but frees the
/// slot it was holding.
fn now_members(conn: &Connection, user_id: i64) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM tasks
         WHERE user_id = ?1 AND is_now = 1 AND parent_id IS NULL
           AND state IN ('open','in_progress')
         ORDER BY id",
    )?;
    stmt.query_map([user_id], |r| r.get(0))?.collect()
}

/// Brings Now back to its cap by dropping the newest members other than `keep`,
/// so a write always takes effect and the task that falls out is the one at the
/// bottom of the list. Returns what it demoted, newest first.
fn trim_now(conn: &Connection, user_id: i64, keep: i64) -> rusqlite::Result<Vec<i64>> {
    let mut members = now_members(conn, user_id)?;
    let mut demoted = Vec::new();
    while members.len() > NOW_CAP {
        let Some(pos) = members.iter().rposition(|id| *id != keep) else { break };
        let id = members.remove(pos);
        conn.execute(
            "UPDATE tasks SET is_now = 0, updated_at = ?1 WHERE id = ?2",
            (now(), id),
        )?;
        demoted.push(id);
    }
    Ok(demoted)
}

/// A person choosing a fourth is told Now is full and nothing moves (the client
/// shows "Now is full — finish or move something first"); an agent's write is
/// trimmed afterwards instead, so it can never fail silently into a full Now.
fn check_now_room(
    conn: &Connection,
    user_id: i64,
    actor: Actor,
    already_in: Option<i64>,
) -> Result<(), UpdateError> {
    if actor == Actor::Agent {
        return Ok(());
    }
    let members = now_members(conn, user_id)?;
    if members.len() < NOW_CAP || already_in.is_some_and(|id| members.contains(&id)) {
        return Ok(());
    }
    Err(UpdateError::NowFull(format!("Now already holds {NOW_CAP} tasks")))
}
```

- [ ] **Step 5: Wire the flag through `create` and `update`**

In `create`, after the `checked_parent` call and before the insert:

```rust
    if new.is_now {
        if new.parent_id.is_some() {
            return Err(UpdateError::InvalidHierarchy(
                "only a top-level task can be in Now".into(),
            ));
        }
        check_now_room(conn, user_id, actor, None)?;
    }
```

and extend the insert and its tail:

```rust
    conn.execute(
        "INSERT INTO tasks
            (user_id, title, source, parent_id, duration_min, duration_source, is_now,
             created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
        (
            user_id,
            &new.title,
            source,
            new.parent_id,
            duration,
            duration_source,
            new.is_now,
            now(),
        ),
    )?;
    let id = conn.last_insert_rowid();
    trim_now(conn, user_id, id)?;
    Ok(conn.query_row(&format!("SELECT {COLS} FROM tasks WHERE id = ?1"), [id], row_to_task)?)
```

In `update`, after the existing hierarchy checks and before the column computation:

```rust
    let parent_id = patch.parent_id.unwrap_or(before.parent_id);
    let is_now = match patch.is_now {
        Some(true) => {
            if parent_id.is_some() {
                return Err(UpdateError::InvalidHierarchy(
                    "only a top-level task can be in Now".into(),
                ));
            }
            check_now_room(conn, user_id, patch.actor, Some(task_id))?;
            true
        }
        Some(false) => false,
        // becoming a step is leaving Now
        None => before.is_now && parent_id.is_none(),
    };
```

(delete the existing `let parent_id = patch.parent_id.unwrap_or(before.parent_id);` line further down, which this replaces), add `is_now = ?8` to the `UPDATE` and shift `updated_at`/`WHERE` to `?9`/`?10` with `is_now` bound between `parent_id` and `now()`, and finish with the trim:

```rust
    let parent = match patch.state {
        Some(_) => cascade(conn, user_id, task_id, parent_id)?,
        None => None,
    };
    let demoted_from_now = trim_now(conn, user_id, task_id)?;
    let task = get(conn, user_id, task_id)?.expect("row was just updated");
    Ok(Some(Updated { task, parent, demoted_from_now }))
```

and extend `Updated`:

```rust
pub struct Updated {
    #[serde(flatten)]
    pub task: Task,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<Task>,
    /// Tasks this write pushed out of Now, newest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub demoted_from_now: Vec<i64>,
}
```

- [ ] **Step 6: Map the new error in `server/src/api.rs`**

```rust
        E::NowFull(m) => {
            (StatusCode::CONFLICT, Json(serde_json::json!({ "error": m }))).into_response()
        }
```

- [ ] **Step 7: Run the tests**

Run: `cargo test -p note-server --test tasks_api`
Expected: PASS, including every pre-existing test in the file.

- [ ] **Step 8: Commit**

```bash
git add server/src/tasks.rs server/src/api.rs server/src/tools/task_ops.rs server/tests/tasks_api.rs
git commit -m "feat(tasks): a stored Now flag capped at three"
```

---

### Task 3: The agent's task tool

**Files:**
- Modify: `server/src/tools/task_ops.rs`
- Modify: `server/src/tools/mod.rs` (the two `describe` arms and `mod tests`)
- Test: `server/src/tools/mod.rs` `mod tests`, `server/tests/tool_invariants.rs`

**Interfaces:**
- Consumes: `tasks::{Actor, NewTask, TaskPatch, NOW_CAP}`.
- Produces: `CreateArgs` gains `#[serde(default)] pub is_now: bool`; `UpdateArgs` gains `pub is_now: Option<bool>`; `task_update` returns `{ task_id, state, is_now, demoted_from_now }`.

- [ ] **Step 1: Write the failing tests**

In `server/src/tools/mod.rs`, inside `mod tests`:

```rust
    #[test]
    fn agent_moves_tasks_in_and_out_of_now() {
        let (conn, tmp) = env();
        let flag = |id: i64| -> i64 {
            conn.query_row("SELECT is_now FROM tasks WHERE id = ?1", [id], |r| r.get(0)).unwrap()
        };
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"email landlord","is_now":true}"#).unwrap();
        let id = out["task_id"].as_i64().unwrap();
        assert_eq!(flag(id), 1);

        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{id},"is_now":false}}"#)).unwrap();
        assert_eq!(out["is_now"], false);
        assert_eq!(flag(id), 0);

        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{id},"is_now":true}}"#)).unwrap();
        assert_eq!(out["is_now"], true);
        assert_eq!(out["demoted_from_now"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_fourth_agent_write_pushes_the_newest_task_out_of_now() {
        let (conn, tmp) = env();
        let mut ids = Vec::new();
        for title in ["a", "b", "c", "d"] {
            let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
                &format!(r#"{{"title":"{title}","is_now":true}}"#)).unwrap();
            ids.push(out["task_id"].as_i64().unwrap());
        }
        let live: Vec<i64> = {
            let mut stmt = conn.prepare(
                "SELECT id FROM tasks WHERE is_now = 1 AND state IN ('open','in_progress') ORDER BY id",
            ).unwrap();
            stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
        };
        assert_eq!(live, vec![ids[0], ids[1], ids[3]], "the newest already in Now stepped aside");

        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{},"is_now":true}}"#, ids[2])).unwrap();
        assert_eq!(out["demoted_from_now"][0], ids[3]);
    }

    #[test]
    fn the_agent_cannot_put_a_step_in_now() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"email landlord"}"#).unwrap();
        let id = out["task_id"].as_i64().unwrap();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_split",
            &format!(r#"{{"task_id":{id},"steps":[
                {{"title":"find the thread","duration_min":5}},
                {{"title":"write and send","duration_min":10}}]}}"#)).unwrap();
        let step = out["step_ids"][0].as_i64().unwrap();
        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{step},"is_now":true}}"#)).unwrap_err();
        assert_eq!(e.kind, "rejected");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p note-server --lib tools::`
Expected: FAIL — `unknown field is_now`.

- [ ] **Step 3: Write the implementation**

In `server/src/tools/task_ops.rs`, add to `CreateArgs`:

```rust
    /// Put the task straight in Now, the user's short list of at most 3.
    #[serde(default)]
    pub is_now: bool,
```

pass it through the `NewTask` literal in `create`:

```rust
        NewTask {
            title: title.to_owned(),
            duration_min: args.duration_min,
            parent_id: None,
            is_now: args.is_now,
        },
```

add to `UpdateArgs`:

```rust
    /// True moves the task into Now, false moves it back to Later.
    pub is_now: Option<bool>,
```

carry it into the patch and report the displacement in `update`:

```rust
    let patch = TaskPatch {
        title,
        description: args.description,
        state: args.state,
        notes: args.notes,
        duration_min: args.duration_min.map(Some),
        is_now: args.is_now,
        actor: Actor::Agent,
        ..Default::default()
    };
    match crate::tasks::update(conn, ctx.user_id, args.task_id, patch) {
        Ok(Some(t)) => Ok(serde_json::json!({
            "task_id": t.task.id,
            "state": t.task.state,
            "is_now": t.task.is_now,
            "demoted_from_now": t.demoted_from_now,
        })),
        Ok(None) => Err(ToolError::not_found(format!("no task {}", args.task_id))),
        Err(e) => Err(task_error(e)),
    }
```

and add the `NowFull` arm to `task_error` in the same file:

```rust
        UpdateError::InvalidDuration(m) | UpdateError::InvalidHierarchy(m)
        | UpdateError::NowFull(m) => ToolError::rejected(m),
```

In `server/src/tools/mod.rs`, restate the two descriptions so the model knows the cap and what a fourth costs:

```rust
        "task_create" => (
            "Create a new task for the current user. Set is_now to put it straight in Now, \
             the user's short list of at most 3 — a fourth pushes the newest one back to Later.",
            schema::<task_ops::CreateArgs>(),
        ),
        "task_update" => (
            "Update a task's title, description, state, notes, duration (whole 5-minute blocks), \
             or whether it sits in Now — the short list of at most 3, where a fourth pushes the \
             newest one back to Later. Steps are never in Now.",
            schema::<task_ops::UpdateArgs>(),
        ),
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p note-server --lib tools::`
Expected: PASS, including `session_surfaces_are_nested_subsets` and `schemas_cover_the_registry_and_are_objects`.

- [ ] **Step 5: Extend the property test**

In `server/tests/tool_invariants.rs`, add the variant to `Op`:

```rust
    TaskSetNow(i64, bool),
```

to `arb_op()`:

```rust
        (1..6i64, any::<bool>()).prop_map(|(id, f)| Op::TaskSetNow(id, f)),
```

to `apply()`:

```rust
        Op::TaskSetNow(id, f) => ("task_update", format!(r#"{{"task_id":{id},"is_now":{f}}}"#)),
```

and to `assert_invariants`, after the hierarchy and duration block:

```rust
    // 3. Now holds at most three live top-level tasks per user, and no step is
    //    ever in it.
    let live: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks
             WHERE user_id = 1 AND is_now = 1 AND parent_id IS NULL
               AND state IN ('open','in_progress')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(live <= 3, "Now overflowed with {live} tasks");
    let flagged_steps: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE is_now = 1 AND parent_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(flagged_steps, 0, "a step was flagged into Now");
```

(renumber the memory comment that follows from `3.` to `4.`)

- [ ] **Step 6: Run the whole suite**

Run: `cargo test --workspace`
Expected: PASS — more than the 236 baseline, zero failures.

- [ ] **Step 7: Commit**

```bash
git add server/src/tools/ server/tests/tool_invariants.rs
git commit -m "feat(tools): the agent can move a task in and out of Now"
```

---

### Task 4: A timestamp the client can group DONE TODAY by

**Files:**
- Modify: `server/src/tasks.rs`
- Test: `server/tests/tasks_api.rs`

**Interfaces:**
- Produces: `Task` gains `pub updated_at: String` (RFC 3339, from `jiff::Timestamp::now()`).

Spec 4.1's third group is "Tasks completed today only … Older done tasks are not shown". `GET /api/tasks` currently carries no timestamp at all, so the client cannot tell a task finished this morning from one finished last month. This is the smallest additive field that unblocks it.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn tasks_carry_the_time_they_last_changed() {
    let (app, cookie, _tmp) = app_with_user().await;
    let (_, t) = post(&app, &cookie, "/api/tasks", r#"{"title":"call dentist"}"#).await;
    let created = t["updated_at"].as_str().unwrap().to_string();
    assert!(created.parse::<jiff::Timestamp>().is_ok(), "not a timestamp: {created}");

    let (_, t) = patch_task(&app, &cookie, 1, r#"{"state":"done"}"#).await;
    let done = t["updated_at"].as_str().unwrap();
    assert!(done >= created.as_str(), "finishing a task did not move its timestamp");
    assert_eq!(list(&app, &cookie).await[0]["updated_at"], done);
}
```

`jiff` is already a dependency of the server crate; add it to `[dev-dependencies]` in `server/Cargo.toml` only if `cargo test` reports it missing for the integration test target.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p note-server --test tasks_api tasks_carry_the_time`
Expected: FAIL — `updated_at` is null.

- [ ] **Step 3: Write the implementation**

In `server/src/tasks.rs`, add the field to `Task` after `is_now`, read it in `row_to_task` as index 10, and append it to `COLS`:

```rust
    pub is_now: bool,
    pub updated_at: String,
}
```

```rust
        is_now: r.get(9)?,
        updated_at: r.get(10)?,
    })
}

const COLS: &str = "id, title, description, state, source, notes, duration_min, \
                    duration_source, parent_id, is_now, updated_at";
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p note-server --test tasks_api`
Expected: PASS.

- [ ] **Step 5: Run the whole suite**

Run: `cargo test --workspace`
Expected: PASS, zero failures.

- [ ] **Step 6: Commit**

```bash
git add server/src/tasks.rs server/tests/tasks_api.rs
git commit -m "feat(tasks): expose when a task last changed"
```

---

## Self-review

**Spec coverage.**
- 4.1 "Which tasks are 'now' is a stored per-task flag" → Task 1 (column) and Task 2 (`Task.is_now` on every task JSON, settable via `PATCH`).
- 4.1 "the agent may set it" → Task 3 (`task_create`'s `is_now`, `task_update`'s `is_now`).
- 4.1 "the user moves tasks between Now/Later via the row's ⋯ menu" → the two writes the menu needs are `PATCH {"is_now":true}` and `PATCH {"is_now":false}`; rendering the menu is the UI plan's.
- 4.1 "Attempting to move a 4th into Now … does not move it" → `a_fourth_task_is_refused_entry_to_now` (409, nothing displaced).
- Acceptance "Now never renders more than 3 tasks, including after agent writes (excess falls to Later, newest-demoted-first)" → `trim_now`, `a_fourth_agent_write_pushes_the_newest_task_out_of_now`, and the standing property-test invariant.
- Acceptance "Done + Undo round-trips a task to its exact prior group and state" → the flag survives `done` (`a_fourth_task_is_refused_entry_to_now`'s tail) and reopening restores the group without failing (`reopening_a_done_now_task_demotes_the_newest_instead_of_failing`).
- Assignment item 5, steps never carry the flag → refused in `create` and `update`, cleared on reparenting, and backed by the v7 CHECK: Tasks 1 and 2, `a_step_can_never_be_in_now`.
- 4.2 toast/undo, 4.3 add row, 4.4 the `in progress` tag → client-side; the data each needs (`state`, `is_now`, `updated_at`) is in the shape from Tasks 2 and 4.
- Empty-state and header copy → client-side; this plan adds no user-facing string except the `error` body behind the fixed toast.

**Placeholders:** none — every step carries the code it asks for.

**Type consistency:** `Actor` (renamed from `DurationActor`, same variants) is used in Tasks 2 and 3; `NOW_CAP` is defined in Task 2 and referenced in Task 3's tests only as the literal 3 the spec fixes; `Updated { task, parent, demoted_from_now }` is produced by `update` and consumed by `api::tasks_update` and `task_ops::update`; `Task.is_now` is a `bool` on the wire in every position (top-level, `children`, and `parent`).
