use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const STATES: &[&str] = &["open", "in_progress", "done", "dropped"];

/// Distinguishes a bad request (invalid `state`) from an infrastructure failure,
/// so callers can map them to different HTTP statuses.
#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("invalid state: {0}")]
    InvalidState(String),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

#[derive(Debug, Serialize)]
pub struct Task {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub state: String,
    pub source: String,
    pub notes: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub state: Option<String>,
    pub notes: Option<String>,
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
    })
}

const COLS: &str = "id, title, description, state, source, notes";

pub fn create(conn: &Connection, user_id: i64, title: &str, source: &str) -> Result<Task> {
    conn.execute(
        "INSERT INTO tasks (user_id, title, source, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?4)",
        (user_id, title, source, now()),
    )?;
    let id = conn.last_insert_rowid();
    Ok(conn.query_row(
        &format!("SELECT {COLS} FROM tasks WHERE id = ?1"),
        [id],
        row_to_task,
    )?)
}

pub fn list(conn: &Connection, user_id: i64) -> Result<Vec<Task>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM tasks WHERE user_id = ?1 AND state != 'dropped' ORDER BY id"
    ))?;
    let rows = stmt.query_map([user_id], row_to_task)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Returns `Ok(None)` when `task_id` doesn't exist or isn't owned by `user_id`;
/// `Err(InvalidState)` when `patch.state` is not one of the allowed values;
/// `Err(Db)` on any other (infrastructure) failure.
pub fn update(
    conn: &Connection,
    user_id: i64,
    task_id: i64,
    patch: TaskPatch,
) -> std::result::Result<Option<Task>, UpdateError> {
    if let Some(s) = &patch.state {
        if !STATES.contains(&s.as_str()) {
            return Err(UpdateError::InvalidState(s.clone()));
        }
    }
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM tasks WHERE id = ?1 AND user_id = ?2",
            (task_id, user_id),
            |r| r.get(0),
        )
        .optional()?;
    if existing.is_none() {
        return Ok(None);
    }
    conn.execute(
        "UPDATE tasks SET
            title = COALESCE(?1, title),
            description = COALESCE(?2, description),
            state = COALESCE(?3, state),
            notes = COALESCE(?4, notes),
            updated_at = ?5
         WHERE id = ?6",
        (&patch.title, &patch.description, &patch.state, &patch.notes, now(), task_id),
    )?;
    Ok(Some(conn.query_row(
        &format!("SELECT {COLS} FROM tasks WHERE id = ?1"),
        [task_id],
        row_to_task,
    )?))
}
