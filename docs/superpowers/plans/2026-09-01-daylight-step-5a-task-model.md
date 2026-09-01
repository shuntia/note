# Daylight Step 5a — Task durations and steps (server) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give a Task a rough duration and exactly-one-level-deep steps, in the model, the HTTP API, and the agent's task tools, so the Tasks UI (step 5.3/5.4, a separate plan) has a contract to code against.

**Architecture:** `tasks.rs` stays the single owner of task invariants — duration granularity, one-level hierarchy, and the parent/child state cascade all live there, so the HTTP surface and the tool surface cannot drift. `api.rs` and `tools/task_ops.rs` are thin adapters that map `tasks::` errors onto their own vocabulary (HTTP 422 / typed `ToolError`). Schema changes ride the existing `MIGRATIONS` array as a new v6 step.

**Tech Stack:** Rust, axum, rusqlite (SQLite), serde/schemars, tokio test harness.

**Spec:** `docs/superpowers/plans/2026-09-01-daylight-ui-spec.md` — Step 5, items 5.1 and 5.2. Items 5.3/5.4 (React rendering) are explicitly out of scope here.

## Global Constraints

From the spec's "Global constraints", the ones that bind server work:

- Copy rules: sentence case; active voice; user vocabulary, never system vocabulary; no tool names in user-facing text. (Applies to any string this plan adds that a user could read. This plan adds none — all new strings are error messages read by the model or by developers.)
- Dropped items are recorded quietly and stay past tense — the model keeps `dropped` as a state, never a deletion.
- Additive-only API: the shipped UI uses `GET/POST /api/tasks` and `PATCH /api/tasks/{id}`; new fields must be optional and must not change the meaning of existing ones.
- Durations are only ever meaningful in 5-minute increments, enforced server-side rather than trusting the client.
- Hierarchy is EXACTLY one level deep — a task with a parent cannot itself be a parent; the API rejects deeper nesting with 422.

---

## Design decisions (argued from the spec)

**Why reject, not round, a non-multiple-of-5 duration.** Spec 5.1 calls the granularity part of the model, and the acceptance box "Durations only ever display in 5-minute increments" is a display consequence of a model guarantee. Rounding hides a caller bug and makes the stored value differ from the value the caller believes it wrote; the UI only ever offers 5-minute steps, so a non-multiple is always a defect, not a user intent. Rejection is uniform across both surfaces: HTTP 422, and a `rejected` `ToolError` the model can immediately correct. The rule is additionally enforced by a SQLite CHECK, so no code path can write a value the UI cannot render.

**Why `duration_source` is derived from the writer, not accepted from the caller.** Spec 5.1 lists `user | agent | none`, and 5.3 renders `Note will estimate` exactly when the source is `none`. If the client could claim `agent`, the chip would lie. So the HTTP surface always writes `user`, the tool surface always writes `agent`, and `none` is the absence of a duration. `TaskPatch` carries the actor in a `#[serde(skip)]` field so the wire shape stays exactly `{"duration_min": …}`.

**Why `GET /api/tasks` nests children instead of listing them flat.** Spec 5.3 renders a parent row with an indented child list and a `≈ <total> min · Note split this into <n> steps · <k> done` sub-line. A flat list would force the client to re-group and would make the *shipped* (step-4-and-earlier) UI render steps as if they were top-level tasks. Nesting is additive today because no task has a parent yet.

**Why `POST /api/tasks/{id}/flatten` returns the removed children.** Spec 5.2's `Keep as one task` must be one call, and the step 5 acceptance box says it is undoable. Children are hard-deleted (a step is a suggestion, not a commitment — leaving `dropped` rows would put "dropped" steps into the user's quiet past-tense history, which the global constraints reserve for things the user actually dropped). Undo therefore needs the removed rows back: the response carries them, and `POST /api/tasks/{id}/split` re-creates them in one call.

**Why the parent/child state cascade lives in `tasks::update`.** Spec 5.3: "Completing all children completes the parent" and "undo reopens parent and the last child". Doing this in the client would make the two writes racy and would leave the tool surface inconsistent. The invariant is: a parent with children is `done` exactly when every non-dropped child is `done`.

---

## File Structure

- `server/src/db.rs` — add migration v6 (duration columns + parent index + CHECKs) and its migration test. Existing file; the `MIGRATIONS` array is the established pattern.
- `server/src/tasks.rs` — the model: new fields on `Task`, `TaskNode` for listing, duration/hierarchy validation, `flatten`, `split`, the state cascade. This is where all the new logic belongs; it is currently 116 lines and stays comfortably readable.
- `server/src/api.rs` — three thin handler changes plus two new routes.
- `server/src/tools/task_ops.rs` — `duration_min` on create/update, new `SplitArgs`/`split`.
- `server/src/tools/mod.rs` — register and describe `task_split`.
- `server/tests/tasks_api.rs` — HTTP-level tests for every new endpoint and rejection.
- `server/tests/tool_invariants.rs` — property test gains split/duration operations and the one-level-deep invariant.

---

### Task 1: Schema — v6 migration

**Files:**
- Modify: `server/src/db.rs` (the `MIGRATIONS` array, and the `mod tests` block)

**Interfaces:**
- Produces: `tasks.duration_min INTEGER NULL`, `tasks.duration_source TEXT NOT NULL DEFAULT 'none'`, index `idx_tasks_parent`. `tasks.parent_id INTEGER REFERENCES tasks(id)` already exists from v1 — do not re-add it.

- [ ] **Step 1: Write the failing test**

In `server/src/db.rs`, inside `mod tests`:

```rust
    #[test]
    fn v6_adds_duration_columns_with_five_minute_check() {
        let conn = open_memory().unwrap();
        conn.execute(
            "INSERT INTO users (username, pass_hash, role) VALUES ('a','h','member')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (user_id, title, created_at, updated_at)
             VALUES (1, 't', 'now', 'now')",
            [],
        )
        .unwrap();
        let (dur, src): (Option<i64>, String) = conn
            .query_row("SELECT duration_min, duration_source FROM tasks WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(dur, None);
        assert_eq!(src, "none");
        assert!(conn.execute("UPDATE tasks SET duration_min = 7 WHERE id = 1", []).is_err());
        assert!(conn.execute("UPDATE tasks SET duration_min = 0 WHERE id = 1", []).is_err());
        assert!(conn.execute("UPDATE tasks SET duration_source = 'guess' WHERE id = 1", []).is_err());
        conn.execute("UPDATE tasks SET duration_min = 20, duration_source = 'agent' WHERE id = 1", [])
            .unwrap();
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p note-server --lib db::tests::v6 -- --nocapture`
Expected: FAIL — `no such column: duration_min`.

- [ ] **Step 3: Write minimal implementation**

Append to `MIGRATIONS` in `server/src/db.rs`, after the v5 string:

```rust
    // v6
    "
    ALTER TABLE tasks ADD COLUMN duration_min INTEGER
        CHECK (duration_min IS NULL OR (duration_min > 0 AND duration_min % 5 = 0));
    ALTER TABLE tasks ADD COLUMN duration_source TEXT NOT NULL DEFAULT 'none'
        CHECK (duration_source IN ('user','agent','none'));
    CREATE INDEX idx_tasks_parent ON tasks(parent_id);
    ",
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p note-server --lib db::`
Expected: PASS, including `migrations_apply_and_are_idempotent` (which asserts `user_version == MIGRATIONS.len()`).

- [ ] **Step 5: Commit**

```bash
git add server/src/db.rs
git commit -m "feat(db): task duration columns and parent index"
```

---

### Task 2: Model — duration on create/patch, one-level hierarchy

**Files:**
- Modify: `server/src/tasks.rs`
- Modify: `server/src/api.rs:124-162` (request structs and the three task handlers)
- Test: `server/tests/tasks_api.rs`

**Interfaces:**
- Produces:
  - `pub struct Task { id, title, description, state, source, notes, duration_min: Option<u32>, duration_source: String, parent_id: Option<i64> }`
  - `pub enum DurationActor { User, Agent }` (`Default` = `User`)
  - `pub struct TaskPatch { title, description, state, notes, duration_min: Option<Option<u32>>, parent_id: Option<Option<i64>>, duration_actor: DurationActor /* #[serde(skip)] */ }`
  - `pub struct NewTask { pub title: String, pub duration_min: Option<u32>, pub parent_id: Option<i64> }`
  - `pub fn create(conn, user_id, new: NewTask, source: &str, actor: DurationActor) -> Result<Task, UpdateError>`
  - `UpdateError::{InvalidState, InvalidDuration, InvalidHierarchy, Db}`
- Consumes: Task 1's columns.

- [ ] **Step 1: Write the failing tests**

Add to `server/tests/tasks_api.rs`. The file already has a local `login` helper; reuse it and add this builder above the new tests:

```rust
async fn app_with_user() -> (axum::Router, String, tempfile::TempDir) {
    let conn = db::open_memory().unwrap();
    auth::create_user(&conn, "aki", "pw", false).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let app = api::router(AppState::new(conn, tmp.path().to_path_buf(), tmp.path().to_path_buf()));
    let cookie = login(&app, "aki", "pw").await;
    (app, cookie, tmp)
}

async fn post(app: &axum::Router, cookie: &str, path: &str, body: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .clone()
        .oneshot(
            Request::post(path)
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

async fn patch_task(app: &axum::Router, cookie: &str, id: i64, body: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .clone()
        .oneshot(
            Request::patch(format!("/api/tasks/{id}"))
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

async fn list(app: &axum::Router, cookie: &str) -> serde_json::Value {
    let res = app
        .clone()
        .oneshot(Request::get("/api/tasks").header(header::COOKIE, cookie).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn task_defaults_carry_no_duration_and_no_parent() {
    let (app, cookie, _tmp) = app_with_user().await;
    let (status, t) = post(&app, &cookie, "/api/tasks", r#"{"title":"call dentist"}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert!(t["duration_min"].is_null());
    assert_eq!(t["duration_source"], "none");
    assert!(t["parent_id"].is_null());
}

#[tokio::test]
async fn duration_is_five_minute_granular_and_user_sourced() {
    let (app, cookie, _tmp) = app_with_user().await;
    let (_, t) = post(&app, &cookie, "/api/tasks", r#"{"title":"email landlord","duration_min":20}"#).await;
    assert_eq!(t["duration_min"], 20);
    assert_eq!(t["duration_source"], "user");

    let (status, _) = post(&app, &cookie, "/api/tasks", r#"{"title":"odd","duration_min":7}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = patch_task(&app, &cookie, 1, r#"{"duration_min":23}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = patch_task(&app, &cookie, 1, r#"{"duration_min":0}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, t) = patch_task(&app, &cookie, 1, r#"{"duration_min":null}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert!(t["duration_min"].is_null());
    assert_eq!(t["duration_source"], "none");
}

#[tokio::test]
async fn grandchild_task_is_rejected() {
    let (app, cookie, _tmp) = app_with_user().await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"parent"}"#).await;
    let (status, child) = post(&app, &cookie, "/api/tasks", r#"{"title":"step","parent_id":1,"duration_min":5}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(child["parent_id"], 1);

    let (status, _) = post(&app, &cookie, "/api/tasks", r#"{"title":"deeper","parent_id":2}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    post(&app, &cookie, "/api/tasks", r#"{"title":"loose"}"#).await;
    let (status, _) = patch_task(&app, &cookie, 3, r#"{"parent_id":2}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // a task that already has children cannot become a child
    let (status, _) = patch_task(&app, &cookie, 1, r#"{"parent_id":3}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // nor its own child
    let (status, _) = patch_task(&app, &cookie, 3, r#"{"parent_id":3}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // nor a task belonging to someone else / not existing at all
    let (status, _) = patch_task(&app, &cookie, 3, r#"{"parent_id":999}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn list_nests_children_under_their_parent() {
    let (app, cookie, _tmp) = app_with_user().await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"email landlord"}"#).await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"find the thread","parent_id":1,"duration_min":5}"#).await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"refill meds"}"#).await;

    let v = list(&app, &cookie).await;
    assert_eq!(v.as_array().unwrap().len(), 2);
    assert_eq!(v[0]["title"], "email landlord");
    assert_eq!(v[0]["children"][0]["title"], "find the thread");
    assert_eq!(v[0]["children"][0]["duration_min"], 5);
    assert_eq!(v[1]["title"], "refill meds");
    assert_eq!(v[1]["children"].as_array().unwrap().len(), 0);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p note-server --test tasks_api`
Expected: FAIL — the new fields do not deserialize (`unknown field`) / are absent from the response.

- [ ] **Step 3: Write the implementation**

`server/src/tasks.rs` — replace the whole file body with:

```rust
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

const STATES: &[&str] = &["open", "in_progress", "done", "dropped"];
const DURATION_STEP_MIN: u32 = 5;
const MAX_DURATION_MIN: u32 = 24 * 60;

/// Separates the caller's mistakes — each with its own HTTP status — from an
/// infrastructure failure.
#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("invalid state: {0}")]
    InvalidState(String),
    #[error("{0}")]
    InvalidDuration(String),
    #[error("{0}")]
    InvalidHierarchy(String),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// A duration's provenance is the writer's identity, never the caller's claim:
/// the HTTP surface is always the user, the agent's tools are always the agent.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum DurationActor {
    #[default]
    User,
    Agent,
}

impl DurationActor {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Task {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub state: String,
    pub source: String,
    pub notes: String,
    pub duration_min: Option<u32>,
    pub duration_source: String,
    pub parent_id: Option<i64>,
}

/// One top-level task with its steps; `children` is always present so the
/// client never has to distinguish "no steps" from "field missing".
#[derive(Debug, Serialize)]
pub struct TaskNode {
    #[serde(flatten)]
    pub task: Task,
    pub children: Vec<Task>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewTask {
    pub title: String,
    #[serde(default)]
    pub duration_min: Option<u32>,
    #[serde(default)]
    pub parent_id: Option<i64>,
}

/// `Option<Option<T>>` fields separate "absent, leave alone" (`None`) from
/// "explicit null, clear it" (`Some(None)`).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub state: Option<String>,
    pub notes: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub duration_min: Option<Option<u32>>,
    #[serde(default, deserialize_with = "present")]
    pub parent_id: Option<Option<i64>>,
    #[serde(skip)]
    pub duration_actor: DurationActor,
}

fn present<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(d).map(Some)
}

fn now() -> String {
    jiff::Timestamp::now().to_string()
}

fn row_to_task(r: &rusqlite::Row) -> rusqlite::Result<Task> {
    Ok(Task {
        id: r.get(0)?,
        title: r.get(1)?,
        description: r.get(2)?,
        state: r.get(3)?,
        source: r.get(4)?,
        notes: r.get(5)?,
        duration_min: r.get(6)?,
        duration_source: r.get(7)?,
        parent_id: r.get(8)?,
    })
}

const COLS: &str =
    "id, title, description, state, source, notes, duration_min, duration_source, parent_id";

fn checked_duration(min: u32) -> Result<u32, UpdateError> {
    if min == 0 || min % DURATION_STEP_MIN != 0 || min > MAX_DURATION_MIN {
        return Err(UpdateError::InvalidDuration(format!(
            "duration_min must be a multiple of {DURATION_STEP_MIN} between {DURATION_STEP_MIN} and {MAX_DURATION_MIN}"
        )));
    }
    Ok(min)
}

fn has_children(conn: &Connection, task_id: i64) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM tasks WHERE parent_id = ?1 AND state != 'dropped')",
        [task_id],
        |r| r.get(0),
    )
}

/// A parent must be the caller's own, must not itself be a child, and must not
/// be the task being reparented.
fn checked_parent(
    conn: &Connection,
    user_id: i64,
    parent_id: i64,
    child_id: Option<i64>,
) -> Result<(), UpdateError> {
    if child_id == Some(parent_id) {
        return Err(UpdateError::InvalidHierarchy("a task cannot be its own step".into()));
    }
    let grandparent: Option<Option<i64>> = conn
        .query_row(
            "SELECT parent_id FROM tasks WHERE id = ?1 AND user_id = ?2",
            (parent_id, user_id),
            |r| r.get(0),
        )
        .optional()?;
    match grandparent {
        None => Err(UpdateError::InvalidHierarchy(format!("no task {parent_id}"))),
        Some(Some(_)) => Err(UpdateError::InvalidHierarchy(
            "steps are one level deep: a step cannot have steps of its own".into(),
        )),
        Some(None) => Ok(()),
    }
}

pub fn create(
    conn: &Connection,
    user_id: i64,
    new: NewTask,
    source: &str,
    actor: DurationActor,
) -> Result<Task, UpdateError> {
    let duration = new.duration_min.map(checked_duration).transpose()?;
    if let Some(p) = new.parent_id {
        checked_parent(conn, user_id, p, None)?;
    }
    let duration_source = if duration.is_some() { actor.as_str() } else { "none" };
    conn.execute(
        "INSERT INTO tasks (user_id, title, source, parent_id, duration_min, duration_source, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        (user_id, &new.title, source, new.parent_id, duration, duration_source, now()),
    )?;
    let id = conn.last_insert_rowid();
    Ok(conn.query_row(&format!("SELECT {COLS} FROM tasks WHERE id = ?1"), [id], row_to_task)?)
}

fn children_of(conn: &Connection, parent_id: i64) -> rusqlite::Result<Vec<Task>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM tasks WHERE parent_id = ?1 AND state != 'dropped' ORDER BY id"
    ))?;
    stmt.query_map([parent_id], row_to_task)?.collect()
}

pub fn list(conn: &Connection, user_id: i64) -> rusqlite::Result<Vec<TaskNode>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM tasks
         WHERE user_id = ?1 AND state != 'dropped' AND parent_id IS NULL ORDER BY id"
    ))?;
    let parents: Vec<Task> = stmt.query_map([user_id], row_to_task)?.collect::<rusqlite::Result<_>>()?;
    parents
        .into_iter()
        .map(|task| {
            let children = children_of(conn, task.id)?;
            Ok(TaskNode { task, children })
        })
        .collect()
}

pub fn get(conn: &Connection, user_id: i64, task_id: i64) -> rusqlite::Result<Option<Task>> {
    conn.query_row(
        &format!("SELECT {COLS} FROM tasks WHERE id = ?1 AND user_id = ?2"),
        (task_id, user_id),
        row_to_task,
    )
    .optional()
}
```

Then the `update` function, which validates, writes, and cascades. Append to `server/src/tasks.rs`:

```rust
/// The result of a patch: the task itself, plus its parent when completing or
/// reopening this step changed the parent's state.
#[derive(Debug, Serialize)]
pub struct Updated {
    #[serde(flatten)]
    pub task: Task,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<Task>,
}

/// Returns `Ok(None)` when `task_id` doesn't exist or isn't owned by `user_id`.
pub fn update(
    conn: &Connection,
    user_id: i64,
    task_id: i64,
    patch: TaskPatch,
) -> Result<Option<Updated>, UpdateError> {
    if let Some(s) = &patch.state {
        if !STATES.contains(&s.as_str()) {
            return Err(UpdateError::InvalidState(s.clone()));
        }
    }
    let duration = match patch.duration_min {
        Some(Some(m)) => Some(Some(checked_duration(m)?)),
        other => other,
    };
    let Some(before) = get(conn, user_id, task_id)? else { return Ok(None) };
    if let Some(Some(p)) = patch.parent_id {
        if has_children(conn, task_id)? {
            return Err(UpdateError::InvalidHierarchy(
                "steps are one level deep: a task with steps cannot become a step".into(),
            ));
        }
        checked_parent(conn, user_id, p, Some(task_id))?;
    }
    let (duration_min, duration_source) = match duration {
        None => (before.duration_min, before.duration_source.clone()),
        Some(None) => (None, "none".to_string()),
        Some(Some(m)) => (Some(m), patch.duration_actor.as_str().to_string()),
    };
    let parent_id = match patch.parent_id {
        None => before.parent_id,
        Some(p) => p,
    };
    conn.execute(
        "UPDATE tasks SET
            title = COALESCE(?1, title),
            description = COALESCE(?2, description),
            state = COALESCE(?3, state),
            notes = COALESCE(?4, notes),
            duration_min = ?5,
            duration_source = ?6,
            parent_id = ?7,
            updated_at = ?8
         WHERE id = ?9",
        (
            &patch.title,
            &patch.description,
            &patch.state,
            &patch.notes,
            duration_min,
            duration_source,
            parent_id,
            now(),
            task_id,
        ),
    )?;
    let parent = match patch.state {
        Some(_) => cascade(conn, user_id, task_id, parent_id)?,
        None => None,
    };
    let task = get(conn, user_id, task_id)?.expect("row was just updated");
    Ok(Some(Updated { task, parent }))
}
```

- [ ] **Step 4: Adapt `api.rs`**

In `server/src/api.rs`, delete the local `CreateTaskReq` struct and rewrite the three handlers:

```rust
async fn tasks_list(user: CurrentUser, State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::tasks::list(&conn, user.id) {
        Ok(ts) => Json(ts).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn tasks_create(
    user: CurrentUser,
    State(state): State<AppState>,
    Json(req): Json<crate::tasks::NewTask>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::tasks::create(&conn, user.id, req, "manual", crate::tasks::DurationActor::User) {
        Ok(t) => Json(t).into_response(),
        Err(e) => task_error(e),
    }
}

async fn tasks_update(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(patch): Json<crate::tasks::TaskPatch>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::tasks::update(&conn, user.id, id, patch) {
        Ok(Some(t)) => Json(t).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => task_error(e),
    }
}

fn task_error(e: crate::tasks::UpdateError) -> axum::response::Response {
    use crate::tasks::UpdateError::*;
    match e {
        InvalidState(_) => StatusCode::BAD_REQUEST.into_response(),
        InvalidDuration(m) | InvalidHierarchy(m) => {
            (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": m })))
                .into_response()
        }
        Db(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p note-server --test tasks_api`
Expected: PASS, including the pre-existing `create_list_update_task`, `invalid_state_is_rejected`, `patch_by_non_owner_is_404`.

- [ ] **Step 6: Commit**

```bash
git add server/src/tasks.rs server/src/api.rs server/tests/tasks_api.rs
git commit -m "feat(tasks): durations and one-level steps in the model and API"
```

---

### Task 3: The parent/child state cascade

**Files:**
- Modify: `server/src/tasks.rs` (add `cascade`, called from `update`)
- Test: `server/tests/tasks_api.rs`

**Interfaces:**
- Produces: `fn cascade(conn, user_id, task_id, parent_id: Option<i64>) -> rusqlite::Result<Option<Task>>` — private; its effect is visible as the `parent` field of `Updated`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn completing_the_last_step_completes_the_parent() {
    let (app, cookie, _tmp) = app_with_user().await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"email landlord"}"#).await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"find the thread","parent_id":1,"duration_min":5}"#).await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"write and send","parent_id":1,"duration_min":10}"#).await;

    let (_, t) = patch_task(&app, &cookie, 2, r#"{"state":"done"}"#).await;
    assert_eq!(t["state"], "done");
    assert!(t.get("parent").is_none(), "parent must not change while a step is open");

    let (_, t) = patch_task(&app, &cookie, 3, r#"{"state":"done"}"#).await;
    assert_eq!(t["parent"]["id"], 1);
    assert_eq!(t["parent"]["state"], "done");
    let v = list(&app, &cookie).await;
    assert_eq!(v[0]["state"], "done");

    // undo the last step: the parent reopens in the same response
    let (_, t) = patch_task(&app, &cookie, 3, r#"{"state":"open"}"#).await;
    assert_eq!(t["parent"]["state"], "in_progress");

    // reopening the remaining done step leaves nothing done
    patch_task(&app, &cookie, 2, r#"{"state":"open"}"#).await;
    let v = list(&app, &cookie).await;
    assert_eq!(v[0]["state"], "open");
}

#[tokio::test]
async fn completing_a_parent_completes_its_steps_and_dropping_drops_them() {
    let (app, cookie, _tmp) = app_with_user().await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"email landlord"}"#).await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"find the thread","parent_id":1,"duration_min":5}"#).await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"write and send","parent_id":1,"duration_min":10}"#).await;

    patch_task(&app, &cookie, 1, r#"{"state":"done"}"#).await;
    let v = list(&app, &cookie).await;
    assert_eq!(v[0]["children"][0]["state"], "done");
    assert_eq!(v[0]["children"][1]["state"], "done");

    patch_task(&app, &cookie, 1, r#"{"state":"dropped"}"#).await;
    let v = list(&app, &cookie).await;
    assert_eq!(v.as_array().unwrap().len(), 0);
    // a dropped parent takes its steps with it, so no step is left stranded
    let (_, t) = patch_task(&app, &cookie, 2, r#"{"notes":"x"}"#).await;
    assert_eq!(t["state"], "dropped");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p note-server --test tasks_api`
Expected: FAIL — no `parent` in the patch response; the parent's state is unchanged.

- [ ] **Step 3: Write the implementation**

Add to `server/src/tasks.rs`:

```rust
fn set_state(conn: &Connection, task_id: i64, state: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE tasks SET state = ?1, updated_at = ?2 WHERE id = ?3",
        (state, now(), task_id),
    )?;
    Ok(())
}

/// Keeps a split consistent in both directions: a parent with steps is `done`
/// exactly when every one of its live steps is, and a parent takes its steps
/// with it when it is finished or dropped.
fn cascade(
    conn: &Connection,
    user_id: i64,
    task_id: i64,
    parent_id: Option<i64>,
) -> rusqlite::Result<Option<Task>> {
    if let Some(parent_id) = parent_id {
        let (total, done): (i64, i64) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(state = 'done'), 0)
             FROM tasks WHERE parent_id = ?1 AND state != 'dropped'",
            [parent_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if total == 0 {
            return Ok(None);
        }
        let parent = get(conn, user_id, parent_id)?;
        let Some(parent) = parent else { return Ok(None) };
        let wanted = if done == total {
            "done"
        } else if parent.state == "done" {
            if done > 0 { "in_progress" } else { "open" }
        } else {
            return Ok(None);
        };
        if parent.state == wanted {
            return Ok(None);
        }
        set_state(conn, parent_id, wanted)?;
        return get(conn, user_id, parent_id);
    }
    let state: String =
        conn.query_row("SELECT state FROM tasks WHERE id = ?1", [task_id], |r| r.get(0))?;
    if state == "done" || state == "dropped" {
        conn.execute(
            "UPDATE tasks SET state = ?1, updated_at = ?2
             WHERE parent_id = ?3 AND state != 'dropped' AND state != ?1",
            (&state, now(), task_id),
        )?;
    }
    Ok(None)
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p note-server --test tasks_api`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add server/src/tasks.rs server/tests/tasks_api.rs
git commit -m "feat(tasks): parent and step states stay in sync"
```

---

### Task 4: Split and flatten endpoints

**Files:**
- Modify: `server/src/tasks.rs` (`split`, `flatten`)
- Modify: `server/src/api.rs` (two routes + handlers)
- Test: `server/tests/tasks_api.rs`

**Interfaces:**
- Produces:
  - `pub struct Step { pub title: String, pub duration_min: u32 }`
  - `pub fn split(conn, user_id, task_id, steps: Vec<Step>, actor: DurationActor) -> Result<Option<TaskNode>, UpdateError>`
  - `pub fn flatten(conn, user_id, task_id) -> Result<Option<(TaskNode, Vec<Task>)>, UpdateError>`
  - Routes `POST /api/tasks/{id}/split` and `POST /api/tasks/{id}/flatten`.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn split_creates_steps_and_totals_the_parent_duration() {
    let (app, cookie, _tmp) = app_with_user().await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"email landlord"}"#).await;
    let (status, node) = post(&app, &cookie, "/api/tasks/1/split", r#"{"steps":[
        {"title":"find the last email thread","duration_min":5},
        {"title":"photos of the ceiling","duration_min":5},
        {"title":"write and send","duration_min":10}]}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(node["duration_min"], 20);
    assert_eq!(node["duration_source"], "user");
    assert_eq!(node["children"].as_array().unwrap().len(), 3);
    assert_eq!(node["children"][2]["duration_min"], 10);

    // 2..=5 steps, each a multiple of five, and only on a task that has none
    let (status, _) = post(&app, &cookie, "/api/tasks/1/split", r#"{"steps":[{"title":"a","duration_min":5},{"title":"b","duration_min":5}]}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    post(&app, &cookie, "/api/tasks", r#"{"title":"other"}"#).await;
    let (status, _) = post(&app, &cookie, "/api/tasks/5/split", r#"{"steps":[{"title":"only","duration_min":5}]}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = post(&app, &cookie, "/api/tasks/5/split", r#"{"steps":[{"title":"a","duration_min":5},{"title":"b","duration_min":7}]}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = post(&app, &cookie, "/api/tasks/2/split", r#"{"steps":[{"title":"a","duration_min":5},{"title":"b","duration_min":5}]}"#).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = post(&app, &cookie, "/api/tasks/999/split", r#"{"steps":[{"title":"a","duration_min":5},{"title":"b","duration_min":5}]}"#).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn flatten_removes_every_step_in_one_call_and_hands_them_back() {
    let (app, cookie, _tmp) = app_with_user().await;
    post(&app, &cookie, "/api/tasks", r#"{"title":"email landlord"}"#).await;
    post(&app, &cookie, "/api/tasks/1/split", r#"{"steps":[
        {"title":"find the last email thread","duration_min":5},
        {"title":"photos of the ceiling","duration_min":5},
        {"title":"write and send","duration_min":10}]}"#).await;

    let (status, out) = post(&app, &cookie, "/api/tasks/1/flatten", "{}").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["task"]["id"], 1);
    assert_eq!(out["task"]["duration_min"], 20);
    assert_eq!(out["task"]["children"].as_array().unwrap().len(), 0);
    assert_eq!(out["removed"].as_array().unwrap().len(), 3);
    assert_eq!(out["removed"][0]["title"], "find the last email thread");

    let n: usize = list(&app, &cookie).await.as_array().unwrap().len();
    assert_eq!(n, 1);

    // undo is the inverse call, and flattening a task with no steps is a no-op
    let (status, node) = post(&app, &cookie, "/api/tasks/1/split", r#"{"steps":[
        {"title":"find the last email thread","duration_min":5},
        {"title":"photos of the ceiling","duration_min":5},
        {"title":"write and send","duration_min":10}]}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(node["children"].as_array().unwrap().len(), 3);

    post(&app, &cookie, "/api/tasks", r#"{"title":"loose"}"#).await;
    let (status, out) = post(&app, &cookie, "/api/tasks/8/flatten", "{}").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["removed"].as_array().unwrap().len(), 0);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p note-server --test tasks_api`
Expected: FAIL — 404, the routes do not exist.

- [ ] **Step 3: Write the implementation**

Add to `server/src/tasks.rs`:

```rust
pub const MIN_STEPS: usize = 2;
pub const MAX_STEPS: usize = 5;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub title: String,
    pub duration_min: u32,
}

/// Replaces nothing: a task that already has steps must be flattened first, so
/// a re-split can never silently discard work the user has already ticked off.
pub fn split(
    conn: &Connection,
    user_id: i64,
    task_id: i64,
    steps: Vec<Step>,
    actor: DurationActor,
) -> Result<Option<TaskNode>, UpdateError> {
    if !(MIN_STEPS..=MAX_STEPS).contains(&steps.len()) {
        return Err(UpdateError::InvalidHierarchy(format!(
            "a split needs {MIN_STEPS} to {MAX_STEPS} steps"
        )));
    }
    let Some(parent) = get(conn, user_id, task_id)? else { return Ok(None) };
    if parent.parent_id.is_some() {
        return Err(UpdateError::InvalidHierarchy(
            "steps are one level deep: a step cannot have steps of its own".into(),
        ));
    }
    if has_children(conn, task_id)? {
        return Err(UpdateError::InvalidHierarchy("this task already has steps".into()));
    }
    let mut total: u32 = 0;
    for s in &steps {
        total += checked_duration(s.duration_min)?;
    }
    for s in steps {
        create(
            conn,
            user_id,
            NewTask { title: s.title, duration_min: Some(s.duration_min), parent_id: Some(task_id) },
            &parent.source,
            actor,
        )?;
    }
    conn.execute(
        "UPDATE tasks SET duration_min = ?1, duration_source = ?2, updated_at = ?3 WHERE id = ?4",
        (total, actor.as_str(), now(), task_id),
    )?;
    node(conn, user_id, task_id)
}

/// Returns the parent and the steps that were removed, so the caller can offer
/// an exact undo.
pub fn flatten(
    conn: &Connection,
    user_id: i64,
    task_id: i64,
) -> Result<Option<(TaskNode, Vec<Task>)>, UpdateError> {
    if get(conn, user_id, task_id)?.is_none() {
        return Ok(None);
    }
    let removed = children_of(conn, task_id)?;
    conn.execute("DELETE FROM tasks WHERE parent_id = ?1", [task_id])?;
    let Some(n) = node(conn, user_id, task_id)? else { return Ok(None) };
    Ok(Some((n, removed)))
}

fn node(conn: &Connection, user_id: i64, task_id: i64) -> Result<Option<TaskNode>, UpdateError> {
    let Some(task) = get(conn, user_id, task_id)? else { return Ok(None) };
    let children = children_of(conn, task.id)?;
    Ok(Some(TaskNode { task, children }))
}
```

In `server/src/api.rs`, register the routes next to the existing task routes:

```rust
        .route("/api/tasks/{id}/split", post(task_split))
        .route("/api/tasks/{id}/flatten", post(task_flatten))
```

and add the handlers beside `tasks_update`:

```rust
#[derive(Deserialize)]
struct SplitReq {
    steps: Vec<crate::tasks::Step>,
}

async fn task_split(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<SplitReq>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::tasks::split(&conn, user.id, id, req.steps, crate::tasks::DurationActor::User) {
        Ok(Some(n)) => Json(n).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => task_error(e),
    }
}

async fn task_flatten(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let conn = state.db.lock().unwrap();
    match crate::tasks::flatten(&conn, user.id, id) {
        Ok(Some((task, removed))) => {
            Json(serde_json::json!({ "task": task, "removed": removed })).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => task_error(e),
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p note-server --test tasks_api`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add server/src/tasks.rs server/src/api.rs server/tests/tasks_api.rs
git commit -m "feat(api): split a task into steps and keep it as one"
```

---

### Task 5: Agent tools — durations and `task_split`

**Files:**
- Modify: `server/src/tools/task_ops.rs`
- Modify: `server/src/tools/mod.rs` (registries, `describe`, `run`, and the unit tests)
- Test: `server/src/tools/mod.rs` `mod tests`, `server/tests/tool_invariants.rs`

**Interfaces:**
- Consumes: `tasks::{split, Step, DurationActor, NewTask}`.
- Produces: tool `task_split` with args `{ task_id: i64, steps: [{ title: String, duration_min: u32 }] }`; `task_create` and `task_update` gain `duration_min: Option<u32>`.

- [ ] **Step 1: Write the failing tests**

In `server/src/tools/mod.rs` `mod tests`:

```rust
    #[test]
    fn agent_sets_durations_in_five_minute_steps() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"email landlord","duration_min":20}"#).unwrap();
        let id = out["task_id"].as_i64().unwrap();
        let (dur, src): (i64, String) = conn
            .query_row("SELECT duration_min, duration_source FROM tasks WHERE id = ?1", [id],
                |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((dur, src.as_str()), (20, "agent"));

        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{id},"duration_min":23}}"#)).unwrap_err();
        assert_eq!(e.kind, "rejected");

        dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_update",
            &format!(r#"{{"task_id":{id},"duration_min":25}}"#)).unwrap();
        let dur: i64 = conn
            .query_row("SELECT duration_min FROM tasks WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(dur, 25);
    }

    #[test]
    fn task_split_makes_one_level_of_steps() {
        let (conn, tmp) = env();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_create",
            r#"{"title":"email landlord"}"#).unwrap();
        let id = out["task_id"].as_i64().unwrap();
        let out = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_split",
            &format!(r#"{{"task_id":{id},"steps":[
                {{"title":"find the last email thread","duration_min":5}},
                {{"title":"write and send","duration_min":10}}]}}"#)).unwrap();
        assert_eq!(out["duration_min"], 15);
        assert_eq!(out["step_ids"].as_array().unwrap().len(), 2);
        let child = out["step_ids"][0].as_i64().unwrap();

        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_split",
            &format!(r#"{{"task_id":{child},"steps":[
                {{"title":"a","duration_min":5}},{{"title":"b","duration_min":5}}]}}"#)).unwrap_err();
        assert_eq!(e.kind, "rejected");

        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_split",
            &format!(r#"{{"task_id":{id},"steps":[{{"title":"only","duration_min":5}}]}}"#)).unwrap_err();
        assert_eq!(e.kind, "rejected");

        let e = dispatch(&conn, &ctx(&tmp), SessionKind::Talk, "task_split",
            r#"{"task_id":999,"steps":[{"title":"a","duration_min":5},{"title":"b","duration_min":5}]}"#).unwrap_err();
        assert_eq!(e.kind, "not_found");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p note-server --lib tools::`
Expected: FAIL — `unknown field duration_min`, then `unknown_tool: task_split`.

- [ ] **Step 3: Write the implementation**

`server/src/tools/task_ops.rs`: add `duration_min: Option<u32>` to `CreateArgs` and `UpdateArgs`, route both through the model, and add:

```rust
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SplitArgs {
    pub task_id: i64,
    /// 2 to 5 steps, each a whole number of 5-minute blocks.
    pub steps: Vec<crate::tasks::Step>,
}

pub fn split(
    conn: &Connection,
    ctx: &ToolCtx,
    args: SplitArgs,
) -> Result<serde_json::Value, ToolError> {
    for s in &args.steps {
        checked_title(&s.title)?;
    }
    match crate::tasks::split(conn, ctx.user_id, args.task_id, args.steps, DurationActor::Agent) {
        Ok(Some(n)) => Ok(serde_json::json!({
            "task_id": n.task.id,
            "duration_min": n.task.duration_min,
            "step_ids": n.children.iter().map(|c| c.id).collect::<Vec<_>>(),
        })),
        Ok(None) => Err(ToolError::not_found(format!("no task {}", args.task_id))),
        Err(e) => Err(task_error(e)),
    }
}

fn task_error(e: crate::tasks::UpdateError) -> ToolError {
    use crate::tasks::UpdateError::*;
    match e {
        InvalidState(s) => ToolError::rejected(format!("invalid state: {s}")),
        InvalidDuration(m) | InvalidHierarchy(m) => ToolError::rejected(m),
        Db(e) => ToolError::internal(e.to_string()),
    }
}
```

`server/src/tools/mod.rs`: add `"task_split"` to `CHECKIN`, `TALK`, and `NIGHTLY` (the nesting-subset test requires it in all three), a `describe` arm, and a `run` arm:

```rust
        "task_split" => (
            "Split a task into 2-5 short steps, each with a duration in whole 5-minute blocks. \
             Only for a task that has no steps yet.",
            schema::<task_ops::SplitArgs>(),
        ),
```

```rust
        "task_split" => task_ops::split(conn, ctx, parse(raw)?),
```

Update the `task_update` description to mention the duration:
`"Update a task's title, description, state, notes, or duration (whole 5-minute blocks)."`

- [ ] **Step 4: Run the tests**

Run: `cargo test -p note-server --lib tools::`
Expected: PASS, including `session_surfaces_are_nested_subsets` and `schemas_cover_the_registry_and_are_objects`.

- [ ] **Step 5: Extend the property test**

In `server/tests/tool_invariants.rs`, add `Op::TaskSplit(i64)` and `Op::TaskDuration(i64, u32)` variants, generate them, and add an invariant to `assert_invariants`:

```rust
    // 3. Task steps stay exactly one level deep and every duration is a
    //    whole number of 5-minute blocks.
    let mut stmt = conn
        .prepare(
            "SELECT c.id FROM tasks c JOIN tasks p ON c.parent_id = p.id
             WHERE p.parent_id IS NOT NULL",
        )
        .unwrap();
    let deep: Vec<i64> = stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert!(deep.is_empty(), "tasks nested more than one level deep: {deep:?}");
    let mut stmt = conn.prepare("SELECT id, duration_min FROM tasks WHERE duration_min IS NOT NULL").unwrap();
    let durs: Vec<(i64, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for (id, d) in durs {
        assert!(d > 0 && d % 5 == 0, "task {id} has an unusable duration {d}");
    }
```

with the generator additions:

```rust
        (1..6i64).prop_map(Op::TaskSplit),
        (1..6i64, prop_oneof![Just(5u32), Just(10), Just(23), Just(0)])
            .prop_map(|(id, d)| Op::TaskDuration(id, d)),
```

and the `apply` arms:

```rust
        Op::TaskSplit(id) => ("task_split", format!(
            r#"{{"task_id":{id},"steps":[{{"title":"a","duration_min":5}},{{"title":"b","duration_min":10}}]}}"#)),
        Op::TaskDuration(id, d) => ("task_update", format!(r#"{{"task_id":{id},"duration_min":{d}}}"#)),
```

- [ ] **Step 6: Run the whole suite**

Run: `cargo test --workspace`
Expected: PASS — more tests than the 225 baseline, zero failures.

- [ ] **Step 7: Commit**

```bash
git add server/src/tools/ server/tests/tool_invariants.rs
git commit -m "feat(tools): the agent can set durations and split a task into steps"
```

---

## Self-review

**Spec coverage.**
- 5.1 `duration_min` / `duration_source` / `parent_id`, one level deep, 422 on deeper nesting → Tasks 1, 2 (`checked_parent`, `has_children`, `task_error`).
- 5.2 agent sets and adjusts durations at 5-minute granularity; splits into 2–5 steps each with a duration; `Keep as one task` deletes the children and keeps the parent → Tasks 4, 5.
- 5.3/5.4 (rendering, `Start` button) → deliberately out of scope; the data those items need (`children`, per-child `duration_min`, parent total, `<k> done` derivable from child states) is all in the `GET /api/tasks` shape from Task 2.
- Acceptance "API rejects a grandchild task with 422" → `grandchild_task_is_rejected`.
- Acceptance "`Keep as one` removes children in one action, undoable" → `flatten_removes_every_step_in_one_call_and_hands_them_back` (one POST; `removed` + `split` give the exact inverse).
- Acceptance "Durations only ever display in 5-minute increments" → enforced at three layers: `checked_duration`, the SQLite CHECK, and the property test.
- Acceptance "Completing the last child marks the parent done in the same optimistic frame" → `completing_the_last_step_completes_the_parent`; the parent travels back in the same response so the client needs no second round trip.

**Placeholders:** none — every step carries the code it asks for.

**Type consistency:** `DurationActor` (not `DurationSource`, which is the wire string) is used in Tasks 2, 4, 5; `TaskNode { task, children }` is produced by `list`/`split`/`node` and consumed by the API handlers and `task_ops::split`; `Updated { task, parent }` is only produced by `update`.
