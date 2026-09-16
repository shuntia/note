use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

const STATES: &[&str] = &["open", "in_progress", "done", "dropped"];
const DURATION_STEP_MIN: u32 = 5;
const MAX_DURATION_MIN: u32 = 24 * 60;

/// How many tasks Now holds at once — a list short enough to finish.
pub const NOW_CAP: usize = 3;

/// Separates the caller's mistakes — each mapped to its own HTTP status — from
/// an infrastructure failure.
#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("invalid state: {0}")]
    InvalidState(String),
    #[error("{0}")]
    InvalidDuration(String),
    #[error("{0}")]
    InvalidHierarchy(String),
    #[error("{0}")]
    NowFull(String),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// The writer's identity, never the caller's claim: the HTTP surface is always
/// the user, the agent's tools are always the agent. It fixes a duration's
/// provenance, and it decides what an over-full Now does.
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
    pub is_now: bool,
    pub updated_at: String,
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
pub struct NewTask {
    pub title: String,
    #[serde(default)]
    pub duration_min: Option<u32>,
    #[serde(default)]
    pub parent_id: Option<i64>,
    #[serde(default)]
    pub is_now: bool,
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
    pub is_now: Option<bool>,
    #[serde(skip)]
    pub actor: Actor,
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
        is_now: r.get(9)?,
        updated_at: r.get(10)?,
    })
}

const COLS: &str = "id, title, description, state, source, notes, duration_min, \
                    duration_source, parent_id, is_now, updated_at";

fn checked_duration(min: u32) -> Result<u32, UpdateError> {
    if min == 0 || !min.is_multiple_of(DURATION_STEP_MIN) || min > MAX_DURATION_MIN {
        return Err(UpdateError::InvalidDuration(format!(
            "duration_min must be a multiple of {DURATION_STEP_MIN}, from {DURATION_STEP_MIN} to {MAX_DURATION_MIN}"
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

/// A parent must be the caller's own, must not itself be a step, and must not
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
    let rows = stmt.query_map([user_id], |r| r.get(0))?;
    rows.collect()
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
        conn.execute("UPDATE tasks SET is_now = 0, updated_at = ?1 WHERE id = ?2", (now(), id))?;
        demoted.push(id);
    }
    Ok(demoted)
}

/// A person choosing a fourth is told Now is full and nothing moves; an agent's
/// write is trimmed afterwards instead, so it can never fail silently into a
/// full Now.
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

pub fn create(
    conn: &Connection,
    user_id: i64,
    new: NewTask,
    source: &str,
    actor: Actor,
) -> Result<Task, UpdateError> {
    let duration = new.duration_min.map(checked_duration).transpose()?;
    if let Some(p) = new.parent_id {
        checked_parent(conn, user_id, p, None)?;
    }
    if new.is_now {
        if new.parent_id.is_some() {
            return Err(UpdateError::InvalidHierarchy(
                "only a top-level task can be in Now".into(),
            ));
        }
        check_now_room(conn, user_id, actor, None)?;
    }
    let duration_source = if duration.is_some() { actor.as_str() } else { "none" };
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
}

fn children_of(conn: &Connection, parent_id: i64) -> rusqlite::Result<Vec<Task>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM tasks WHERE parent_id = ?1 AND state != 'dropped' ORDER BY id"
    ))?;
    let rows = stmt.query_map([parent_id], row_to_task)?;
    rows.collect()
}

pub fn get(conn: &Connection, user_id: i64, task_id: i64) -> rusqlite::Result<Option<Task>> {
    conn.query_row(
        &format!("SELECT {COLS} FROM tasks WHERE id = ?1 AND user_id = ?2"),
        (task_id, user_id),
        row_to_task,
    )
    .optional()
}

pub fn list(conn: &Connection, user_id: i64) -> rusqlite::Result<Vec<TaskNode>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM tasks
         WHERE user_id = ?1 AND state != 'dropped' AND parent_id IS NULL ORDER BY id"
    ))?;
    let parents: Vec<Task> =
        stmt.query_map([user_id], row_to_task)?.collect::<rusqlite::Result<_>>()?;
    parents
        .into_iter()
        .map(|task| {
            let children = children_of(conn, task.id)?;
            Ok(TaskNode { task, children })
        })
        .collect()
}

pub const MIN_STEPS: usize = 2;
pub const MAX_STEPS: usize = 5;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub title: String,
    pub duration_min: u32,
}

/// Every column of a task row and of its steps, plus their event links, taken
/// verbatim so a session that fails partway can be undone byte for byte.
#[derive(Debug, Clone)]
pub struct Snapshot {
    task_id: i64,
    columns: Vec<String>,
    rows: Vec<Vec<rusqlite::types::Value>>,
    links: Vec<(i64, i64)>,
}

pub fn snapshot(conn: &Connection, user_id: i64, task_id: i64) -> rusqlite::Result<Snapshot> {
    let mut stmt = conn.prepare(
        "SELECT * FROM tasks WHERE user_id = ?1 AND (id = ?2 OR parent_id = ?2)
         ORDER BY parent_id IS NOT NULL, id",
    )?;
    let columns: Vec<String> = stmt.column_names().iter().map(|s| (*s).to_owned()).collect();
    let width = columns.len();
    let rows = stmt
        .query_map((user_id, task_id), |r| {
            (0..width).map(|i| r.get(i)).collect::<rusqlite::Result<Vec<_>>>()
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut stmt = conn.prepare(
        "SELECT event_id, task_id FROM event_tasks
         WHERE task_id = ?1 OR task_id IN (SELECT id FROM tasks WHERE parent_id = ?1)",
    )?;
    let links = stmt
        .query_map([task_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Snapshot { task_id, columns, rows, links })
}

/// Puts the snapshot back in one transaction: steps the session added are gone,
/// steps it removed are back, and every restored row keeps its original id.
pub fn restore(conn: &Connection, snap: &Snapshot) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM event_tasks
         WHERE task_id = ?1 OR task_id IN (SELECT id FROM tasks WHERE parent_id = ?1)",
        [snap.task_id],
    )?;
    tx.execute("DELETE FROM tasks WHERE parent_id = ?1", [snap.task_id])?;
    tx.execute("DELETE FROM tasks WHERE id = ?1", [snap.task_id])?;
    let holes =
        (1..=snap.columns.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ");
    let insert =
        format!("INSERT INTO tasks ({}) VALUES ({holes})", snap.columns.join(", "));
    for row in &snap.rows {
        tx.execute(&insert, rusqlite::params_from_iter(row.iter()))?;
    }
    for (event_id, task_id) in &snap.links {
        tx.execute(
            "INSERT INTO event_tasks (event_id, task_id) VALUES (?1, ?2)",
            (event_id, task_id),
        )?;
    }
    tx.commit()
}

pub fn node(conn: &Connection, user_id: i64, task_id: i64) -> Result<Option<TaskNode>, UpdateError> {
    let Some(task) = get(conn, user_id, task_id)? else { return Ok(None) };
    let children = children_of(conn, task.id)?;
    Ok(Some(TaskNode { task, children }))
}

/// Refuses a task that already has steps, so a re-split can never silently
/// discard work the user has already ticked off; the parent's duration becomes
/// the total of its steps.
pub fn split(
    conn: &Connection,
    user_id: i64,
    task_id: i64,
    steps: Vec<Step>,
    actor: Actor,
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
            NewTask {
                title: s.title,
                duration_min: Some(s.duration_min),
                parent_id: Some(task_id),
                is_now: false,
            },
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

/// Removes the task, its steps, and every event link to them. `false` when the
/// task is not this user's.
pub fn delete(conn: &Connection, user_id: i64, task_id: i64) -> rusqlite::Result<bool> {
    let tx = conn.unchecked_transaction()?;
    let gone = delete_within(&tx, user_id, task_id)?;
    tx.commit()?;
    Ok(gone)
}

/// The deletion itself, for callers that already hold a transaction.
pub(crate) fn delete_within(
    conn: &Connection,
    user_id: i64,
    task_id: i64,
) -> rusqlite::Result<bool> {
    if get(conn, user_id, task_id)?.is_none() {
        return Ok(false);
    }
    conn.execute(
        "DELETE FROM event_tasks
         WHERE task_id = ?1 OR task_id IN (SELECT id FROM tasks WHERE parent_id = ?1)",
        [task_id],
    )?;
    conn.execute("DELETE FROM tasks WHERE parent_id = ?1", [task_id])?;
    conn.execute("DELETE FROM tasks WHERE id = ?1", [task_id])?;
    Ok(true)
}

/// A patched task, plus its parent when finishing or reopening this step also
/// moved the parent, so the client needs no second round trip.
#[derive(Debug, Serialize)]
pub struct Updated {
    #[serde(flatten)]
    pub task: Task,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<Task>,
    /// Tasks this write pushed out of Now, newest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub demoted_from_now: Vec<i64>,
}

fn set_state(conn: &Connection, task_id: i64, state: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE tasks SET state = ?1, updated_at = ?2 WHERE id = ?3",
        (state, now(), task_id),
    )?;
    Ok(())
}

/// Keeps a split consistent in both directions: a parent with steps is `done`
/// exactly when every live step is, and a parent takes its steps with it when
/// it is finished or dropped.
fn cascade(
    conn: &Connection,
    user_id: i64,
    task_id: i64,
    parent_id: Option<i64>,
) -> rusqlite::Result<Option<Task>> {
    let Some(parent_id) = parent_id else {
        let state: String =
            conn.query_row("SELECT state FROM tasks WHERE id = ?1", [task_id], |r| r.get(0))?;
        if state == "done" || state == "dropped" {
            conn.execute(
                "UPDATE tasks SET state = ?1, updated_at = ?2
                 WHERE parent_id = ?3 AND state != 'dropped' AND state != ?1",
                (&state, now(), task_id),
            )?;
        }
        return Ok(None);
    };
    let (total, done): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(state = 'done'), 0)
         FROM tasks WHERE parent_id = ?1 AND state != 'dropped'",
        [parent_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let Some(parent) = get(conn, user_id, parent_id)? else { return Ok(None) };
    let wanted = if total > 0 && done == total {
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
    get(conn, user_id, parent_id)
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
        None => (before.duration_min, before.duration_source),
        Some(None) => (None, "none".to_string()),
        Some(Some(m)) => (Some(m), patch.actor.as_str().to_string()),
    };
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
    conn.execute(
        "UPDATE tasks SET
            title = COALESCE(?1, title),
            description = COALESCE(?2, description),
            state = COALESCE(?3, state),
            notes = COALESCE(?4, notes),
            duration_min = ?5,
            duration_source = ?6,
            parent_id = ?7,
            is_now = ?8,
            updated_at = ?9
         WHERE id = ?10",
        (
            &patch.title,
            &patch.description,
            &patch.state,
            &patch.notes,
            duration_min,
            duration_source,
            parent_id,
            is_now,
            now(),
            task_id,
        ),
    )?;
    let parent = match patch.state {
        Some(_) => cascade(conn, user_id, task_id, parent_id)?,
        None => None,
    };
    let demoted_from_now = trim_now(conn, user_id, task_id)?;
    let task = get(conn, user_id, task_id)?.expect("row was just updated");
    Ok(Some(Updated { task, parent, demoted_from_now }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_user() -> (Connection, i64) {
        let conn = crate::db::open_memory().unwrap();
        let id = crate::auth::create_user(&conn, "aki", "pw", false).unwrap();
        (conn, id)
    }

    fn task(conn: &Connection, uid: i64, title: &str, parent: Option<i64>) -> i64 {
        create(
            conn,
            uid,
            NewTask { title: title.into(), parent_id: parent, ..NewTask::default() },
            "manual",
            Actor::User,
        )
        .unwrap()
        .id
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn delete_removes_a_leaf() {
        let (conn, uid) = db_with_user();
        let id = task(&conn, uid, "solo", None);
        assert!(delete(&conn, uid, id).unwrap());
        assert!(get(&conn, uid, id).unwrap().is_none());
        assert!(!delete(&conn, uid, id).unwrap());
    }

    #[test]
    fn delete_removes_a_parent_with_its_steps_and_event_links() {
        let (conn, uid) = db_with_user();
        let parent = task(&conn, uid, "parent", None);
        let step = task(&conn, uid, "step", Some(parent));
        let other = task(&conn, uid, "other", None);
        conn.execute(
            "INSERT INTO plans (user_id, date, created_at) VALUES (?1, '2026-09-15', 'x')",
            [uid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time) VALUES (1, 'checkin', '09:00')",
            [],
        )
        .unwrap();
        for t in [parent, step, other] {
            conn.execute("INSERT INTO event_tasks (event_id, task_id) VALUES (1, ?1)", [t])
                .unwrap();
        }
        assert!(delete(&conn, uid, parent).unwrap());
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM tasks"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM event_tasks"), 1);
        assert!(get(&conn, uid, other).unwrap().is_some());
    }

    #[test]
    fn delete_ignores_another_users_task() {
        let (conn, uid) = db_with_user();
        let bo = crate::auth::create_user(&conn, "bo", "pw", false).unwrap();
        let theirs = task(&conn, bo, "theirs", None);
        assert!(!delete(&conn, uid, theirs).unwrap());
        assert!(get(&conn, bo, theirs).unwrap().is_some());
    }
}
