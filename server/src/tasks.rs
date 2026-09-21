use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

pub const STATES: &[&str] = &["open", "in_progress", "done", "dropped"];
/// Where a task came from: typed by the user, written by the agent, or
/// mirrored from another system by an importer.
pub const SOURCES: &[&str] = &["manual", "agent", "import"];
/// How the block holding a task announces itself when it starts: silently, in
/// the day's thread, or as a notification.
pub const NOTIFY: &[&str] = &["none", "chat", "notify"];
const DURATION_STEP_MIN: u32 = 5;
const MAX_DURATION_MIN: u32 = 24 * 60;
const MAX_PROGRESS: u32 = 100;
/// The grain every projected figure lands on.
const PROJECTION_STEP_MIN: u32 = 15;
pub const MAX_TITLE_BYTES: usize = 500;
const MAX_TEXT_BYTES: usize = 16 * 1024;
const MAX_EXTERNAL_ID_BYTES: usize = 200;
const MAX_URL_BYTES: usize = 2 * 1024;

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
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    ExternalIdTaken(String),
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
    /// RFC 3339 UTC, like `updated_at`; only a top-level task carries one.
    pub due_at: Option<String>,
    /// The importer's own id for this task, opaque here and unique per user.
    pub external_id: Option<String>,
    pub url: String,
    pub notify: String,
    /// Minutes actually worked on it, summed from the sessions that ended.
    pub actual_min: Option<u32>,
    /// How far along the task is, 0 to 100.
    pub progress: u32,
    /// Where the minutes already worked say the task will land, and what is left
    /// of that. Derived from `progress` and `actual_min`, never stored.
    pub expected_min: Option<u32>,
    pub remaining_min: Option<u32>,
}

/// Rounds to the nearest quarter hour; anything above zero lands on at least one.
fn quantize(minutes: u64) -> u32 {
    if minutes == 0 {
        return 0;
    }
    let step = u64::from(PROJECTION_STEP_MIN);
    let rounded = (minutes + step / 2) / step * step;
    rounded.max(step) as u32
}

/// The total the task is heading for and what is left of it, read off how far it
/// says it has got and how long that took. Progress of nothing, or time of
/// nothing, extrapolates to nothing: both figures are absent.
pub fn projection(progress: u32, actual_min: Option<u32>) -> (Option<u32>, Option<u32>) {
    let spent = u64::from(actual_min.unwrap_or(0));
    let progress = u64::from(progress.min(MAX_PROGRESS));
    if progress == 0 || spent == 0 {
        return (None, None);
    }
    let total = spent * 100 / progress;
    (Some(quantize(total)), Some(quantize(total.saturating_sub(spent))))
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
    pub description: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub duration_min: Option<u32>,
    #[serde(default)]
    pub parent_id: Option<i64>,
    #[serde(default)]
    pub is_now: bool,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub due_at: Option<Option<String>>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub external_id: Option<String>,
    /// Overrides the caller's default source; the routes decide what a
    /// principal may write.
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub notify: Option<String>,
    #[serde(default)]
    pub progress: Option<u32>,
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
    #[serde(default, deserialize_with = "present")]
    pub due_at: Option<Option<String>>,
    pub url: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub external_id: Option<Option<String>>,
    pub notify: Option<String>,
    pub progress: Option<u32>,
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
    let actual_min: Option<u32> = r.get(15)?;
    let progress: u32 = r.get(16)?;
    let (expected_min, remaining_min) = projection(progress, actual_min);
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
        due_at: r.get(11)?,
        external_id: r.get(12)?,
        url: r.get(13)?,
        notify: r.get(14)?,
        actual_min,
        progress,
        expected_min,
        remaining_min,
    })
}

const COLS: &str = "id, title, description, state, source, notes, duration_min, \
                    duration_source, parent_id, is_now, updated_at, due_at, \
                    external_id, url, notify, actual_min, progress";

fn checked_duration(min: u32) -> Result<u32, UpdateError> {
    if min == 0 || !min.is_multiple_of(DURATION_STEP_MIN) || min > MAX_DURATION_MIN {
        return Err(UpdateError::InvalidDuration(format!(
            "duration_min must be a multiple of {DURATION_STEP_MIN}, from {DURATION_STEP_MIN} to {MAX_DURATION_MIN}"
        )));
    }
    Ok(min)
}

fn checked_progress(value: u32) -> Result<u32, UpdateError> {
    if value > MAX_PROGRESS {
        return Err(UpdateError::Invalid(format!("progress must be 0 to {MAX_PROGRESS}")));
    }
    Ok(value)
}

/// A due date is stored the way `updated_at` is, so parsing and ordering are
/// the same everywhere: RFC 3339, normalised to UTC.
fn checked_due(raw: &str) -> Result<String, UpdateError> {
    raw.parse::<jiff::Timestamp>()
        .map(|t| t.to_string())
        .map_err(|_| UpdateError::Invalid(format!("due_at must be an RFC 3339 instant, got {raw:?}")))
}

fn checked_notify(raw: &str) -> Result<String, UpdateError> {
    if !NOTIFY.contains(&raw) {
        return Err(UpdateError::Invalid(format!("notify must be one of {}", NOTIFY.join(", "))));
    }
    Ok(raw.to_owned())
}

fn checked_title(raw: &str) -> Result<String, UpdateError> {
    let title = raw.trim();
    if title.is_empty() || title.len() > MAX_TITLE_BYTES {
        return Err(UpdateError::Invalid(format!("title must be 1..={MAX_TITLE_BYTES} bytes")));
    }
    Ok(title.to_owned())
}

fn checked_text(field: &str, value: &str) -> Result<(), UpdateError> {
    if value.len() > MAX_TEXT_BYTES {
        return Err(UpdateError::Invalid(format!(
            "{field} must be at most {MAX_TEXT_BYTES} bytes"
        )));
    }
    Ok(())
}

fn checked_url(raw: &str) -> Result<String, UpdateError> {
    if raw.len() > MAX_URL_BYTES {
        return Err(UpdateError::Invalid(format!("url must be at most {MAX_URL_BYTES} bytes")));
    }
    Ok(raw.to_owned())
}

pub fn checked_external_id(raw: &str) -> Result<String, UpdateError> {
    let id = raw.trim();
    if id.is_empty() || id.len() > MAX_EXTERNAL_ID_BYTES {
        return Err(UpdateError::Invalid(format!(
            "external_id must be 1..={MAX_EXTERNAL_ID_BYTES} bytes"
        )));
    }
    Ok(id.to_owned())
}

fn checked_source(raw: &str) -> Result<String, UpdateError> {
    if !SOURCES.contains(&raw) {
        return Err(UpdateError::Invalid(format!(
            "source must be one of {}, got {raw:?}",
            SOURCES.join(", ")
        )));
    }
    Ok(raw.to_owned())
}

/// Steps hang under a deadline rather than carrying one of their own, which is
/// also what the schema enforces.
fn checked_due_placement(parent_id: Option<i64>, due_at: Option<&str>) -> Result<(), UpdateError> {
    if parent_id.is_some() && due_at.is_some() {
        return Err(UpdateError::Invalid(
            "a step carries no due date of its own; the task it belongs to holds it".into(),
        ));
    }
    Ok(())
}

/// The id of the task holding this external id, when some other task does.
fn external_id_holder(
    conn: &Connection,
    user_id: i64,
    external_id: &str,
    except: Option<i64>,
) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT id FROM tasks WHERE user_id = ?1 AND external_id = ?2 AND id IS NOT ?3",
        (user_id, external_id, except),
        |r| r.get(0),
    )
    .optional()
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

/// `source` is the caller's default; a body that names one of `SOURCES` wins,
/// which is how an importer marks what it mirrored.
pub fn create(
    conn: &Connection,
    user_id: i64,
    new: NewTask,
    source: &str,
    actor: Actor,
) -> Result<Task, UpdateError> {
    let title = checked_title(&new.title)?;
    let duration = new.duration_min.map(checked_duration).transpose()?;
    let description = new.description.unwrap_or_default();
    let notes = new.notes.unwrap_or_default();
    checked_text("description", &description)?;
    checked_text("notes", &notes)?;
    let due_at = new.due_at.flatten().as_deref().map(checked_due).transpose()?;
    checked_due_placement(new.parent_id, due_at.as_deref())?;
    let state = match new.state.as_deref() {
        Some(s) if !STATES.contains(&s) => return Err(UpdateError::InvalidState(s.to_owned())),
        other => other.unwrap_or("open").to_owned(),
    };
    let url = checked_url(new.url.as_deref().unwrap_or_default())?;
    let external_id = new.external_id.as_deref().map(checked_external_id).transpose()?;
    let source = checked_source(new.source.as_deref().unwrap_or(source))?;
    let notify = new.notify.as_deref().map(checked_notify).transpose()?;
    let progress = new.progress.map(checked_progress).transpose()?.unwrap_or(0);
    if let Some(p) = new.parent_id {
        checked_parent(conn, user_id, p, None)?;
    }
    if let Some(id) = &external_id {
        if let Some(held) = external_id_holder(conn, user_id, id, None)? {
            return Err(UpdateError::ExternalIdTaken(format!(
                "external_id {id} already belongs to task {held}"
            )));
        }
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
            (user_id, title, description, notes, source, state, parent_id, duration_min,
             duration_source, is_now, due_at, url, external_id, notify, created_at, updated_at,
             completed_at, progress)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 COALESCE(?15, 'notify'), ?14, ?14,
                 CASE WHEN ?6 = 'done' THEN ?14 END,
                 CASE WHEN ?6 = 'done' THEN 100 ELSE ?16 END)",
        rusqlite::params![
            user_id,
            &title,
            &description,
            &notes,
            &source,
            &state,
            new.parent_id,
            duration,
            duration_source,
            new.is_now,
            due_at,
            url,
            external_id,
            now(),
            notify,
            progress,
        ],
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

/// How many of the user's tasks and steps were finished inside the half-open
/// span; `completed_at` is RFC3339 UTC, which orders lexically.
pub fn done_between(
    conn: &Connection,
    user_id: i64,
    from: jiff::Timestamp,
    to: jiff::Timestamp,
) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM tasks
         WHERE user_id = ?1 AND state = 'done' AND completed_at >= ?2 AND completed_at < ?3",
        (user_id, from.to_string(), to.to_string()),
        |r| r.get(0),
    )
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
                ..NewTask::default()
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

/// What an upsert did, so the route can say created, updated, or declined.
pub enum Upsert {
    Created(TaskNode),
    Updated(TaskNode),
    /// The user deleted this external task; it is not recreated.
    Declined { deleted_at: String },
}

/// When this external id was buried.
pub fn tombstone(
    conn: &Connection,
    user_id: i64,
    external_id: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT deleted_at FROM task_tombstones WHERE user_id = ?1 AND external_id = ?2",
        (user_id, external_id),
        |r| r.get(0),
    )
    .optional()
}

fn bury(conn: &Connection, user_id: i64, external_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO task_tombstones (user_id, external_id, deleted_at)
         VALUES (?1, ?2, ?3)",
        (user_id, external_id, now()),
    )?;
    Ok(())
}

pub fn by_external(
    conn: &Connection,
    user_id: i64,
    external_id: &str,
) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT id FROM tasks WHERE user_id = ?1 AND external_id = ?2",
        (user_id, external_id),
        |r| r.get(0),
    )
    .optional()
}

/// Mirrors one task from another system, keyed on `external_id`. The importer
/// owns the title, the notes, the due date and the link; the description, the
/// duration and the steps belong to whoever briefed the task, and the state
/// belongs to the user: a dropped task stays dropped and a done one never
/// reopens from the outside.
pub fn upsert(
    conn: &Connection,
    user_id: i64,
    external_id: &str,
    new: NewTask,
) -> Result<Upsert, UpdateError> {
    let external_id = checked_external_id(external_id)?;
    if let Some(deleted_at) = tombstone(conn, user_id, &external_id)? {
        return Ok(Upsert::Declined { deleted_at });
    }
    let Some(id) = by_external(conn, user_id, &external_id)? else {
        let made = create(
            conn,
            user_id,
            NewTask { external_id: Some(external_id), ..new },
            "import",
            Actor::User,
        )?;
        let node = node(conn, user_id, made.id)?.expect("row was just created");
        return Ok(Upsert::Created(node));
    };
    let before = get(conn, user_id, id)?.expect("row was just found");
    if let Some(s) = new.state.as_deref() {
        if !STATES.contains(&s) {
            return Err(UpdateError::InvalidState(s.to_owned()));
        }
    }
    let state = (before.state != "dropped" && before.state != "done"
        && new.state.as_deref() == Some("done"))
        .then(|| "done".to_owned());
    let patch = TaskPatch {
        title: Some(new.title),
        notes: new.notes,
        due_at: new.due_at,
        url: new.url,
        state,
        ..Default::default()
    };
    update(conn, user_id, id, patch)?;
    let node = node(conn, user_id, id)?.expect("row was just updated");
    Ok(Upsert::Updated(node))
}

/// Removes the task the importer knows by this id, burying the id with it.
pub fn delete_by_external(
    conn: &Connection,
    user_id: i64,
    external_id: &str,
) -> rusqlite::Result<bool> {
    match by_external(conn, user_id, external_id)? {
        Some(id) => delete(conn, user_id, id),
        None => Ok(false),
    }
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
/// An imported task leaves a tombstone behind, so the next run of whatever
/// created it sees a decision rather than a task to make again.
pub(crate) fn delete_within(
    conn: &Connection,
    user_id: i64,
    task_id: i64,
) -> rusqlite::Result<bool> {
    let Some(task) = get(conn, user_id, task_id)? else { return Ok(false) };
    if let Some(external_id) = &task.external_id {
        bury(conn, user_id, external_id)?;
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
        "UPDATE tasks SET state = ?1, updated_at = ?2,
             completed_at = CASE WHEN ?1 = 'done' THEN COALESCE(completed_at, ?2) END,
             progress = CASE WHEN ?1 = 'done' THEN 100 ELSE progress END
         WHERE id = ?3",
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
                "UPDATE tasks SET state = ?1, updated_at = ?2,
                     completed_at = CASE WHEN ?1 = 'done' THEN COALESCE(completed_at, ?2) END,
                     progress = CASE WHEN ?1 = 'done' THEN 100 ELSE progress END
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
    let title = match &patch.title {
        Some(t) => Some(checked_title(t)?),
        None => None,
    };
    if let Some(d) = &patch.description {
        checked_text("description", d)?;
    }
    if let Some(n) = &patch.notes {
        checked_text("notes", n)?;
    }
    let due_at = match &patch.due_at {
        Some(Some(raw)) => Some(Some(checked_due(raw)?)),
        other => other.as_ref().map(|_| None),
    };
    let url = match &patch.url {
        Some(u) => Some(checked_url(u)?),
        None => None,
    };
    let external_id = match &patch.external_id {
        Some(Some(raw)) => Some(Some(checked_external_id(raw)?)),
        other => other.as_ref().map(|_| None),
    };
    let notify = patch.notify.as_deref().map(checked_notify).transpose()?;
    let progress = patch.progress.map(checked_progress).transpose()?;
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
    let due_at = due_at.unwrap_or(before.due_at);
    checked_due_placement(parent_id, due_at.as_deref())?;
    let external_id = external_id.unwrap_or(before.external_id);
    if let Some(id) = &external_id {
        if let Some(held) = external_id_holder(conn, user_id, id, Some(task_id))? {
            return Err(UpdateError::ExternalIdTaken(format!(
                "external_id {id} already belongs to task {held}"
            )));
        }
    }
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
            due_at = ?9,
            url = COALESCE(?10, url),
            external_id = ?11,
            notify = COALESCE(?14, notify),
            progress = CASE WHEN COALESCE(?3, state) = 'done' THEN 100
                ELSE COALESCE(?15, progress) END,
            updated_at = ?12,
            completed_at = CASE WHEN COALESCE(?3, state) = 'done'
                THEN COALESCE(completed_at, ?12) END
         WHERE id = ?13",
        rusqlite::params![
            &title,
            &patch.description,
            &patch.state,
            &patch.notes,
            duration_min,
            duration_source,
            parent_id,
            is_now,
            due_at,
            url,
            external_id,
            now(),
            task_id,
            notify,
            progress,
        ],
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
    fn restore_undoes_a_briefing_session_row_for_row() {
        let (conn, uid) = db_with_user();
        let id = task(&conn, uid, "essay", None);
        update(
            &conn,
            uid,
            id,
            TaskPatch {
                description: Some("the caller's own text".into()),
                duration_min: Some(Some(15)),
                ..Default::default()
            },
        )
        .unwrap();
        split(
            &conn,
            uid,
            id,
            vec![
                Step { title: "draft".into(), duration_min: 20 },
                Step { title: "edit".into(), duration_min: 10 },
            ],
            Actor::User,
        )
        .unwrap();
        let before = node(&conn, uid, id).unwrap().unwrap();
        let snap = snapshot(&conn, uid, id).unwrap();

        let step = before.children[0].id;
        delete(&conn, uid, step).unwrap();
        update(
            &conn,
            uid,
            id,
            TaskPatch {
                description: Some("the agent's brief".into()),
                state: Some("dropped".into()),
                ..Default::default()
            },
        )
        .unwrap();
        restore(&conn, &snap).unwrap();

        let after = node(&conn, uid, id).unwrap().unwrap();
        assert_eq!(after.task.description, before.task.description);
        assert_eq!(after.task.state, before.task.state);
        assert_eq!(after.task.duration_min, before.task.duration_min);
        assert_eq!(after.task.updated_at, before.task.updated_at);
        let ids = |n: &TaskNode| n.children.iter().map(|c| c.id).collect::<Vec<_>>();
        assert_eq!(ids(&after), ids(&before));
    }

    #[test]
    fn completion_time_follows_the_state_in_both_directions() {
        let (conn, uid) = db_with_user();
        let id = task(&conn, uid, "write it up", None);
        let completed = |id: i64| -> Option<String> {
            conn.query_row("SELECT completed_at FROM tasks WHERE id = ?1", [id], |r| r.get(0))
                .unwrap()
        };
        assert!(completed(id).is_none());
        update(&conn, uid, id, TaskPatch { state: Some("done".into()), ..Default::default() })
            .unwrap();
        let first = completed(id).expect("done carries a completion time");
        update(&conn, uid, id, TaskPatch { notes: Some("later".into()), ..Default::default() })
            .unwrap();
        assert_eq!(completed(id).as_deref(), Some(first.as_str()), "an unrelated write keeps it");
        update(&conn, uid, id, TaskPatch { state: Some("open".into()), ..Default::default() })
            .unwrap();
        assert!(completed(id).is_none(), "reopening clears it");
    }

    #[test]
    fn done_between_counts_by_completion_time() {
        let (conn, uid) = db_with_user();
        let id = task(&conn, uid, "write it up", None);
        update(&conn, uid, id, TaskPatch { state: Some("done".into()), ..Default::default() })
            .unwrap();
        let now = jiff::Timestamp::now();
        let hour = jiff::Span::new().hours(1);
        assert_eq!(done_between(&conn, uid, now.checked_sub(hour).unwrap(), now.checked_add(hour).unwrap()).unwrap(), 1);
        assert_eq!(
            done_between(&conn, uid, now.checked_add(hour).unwrap(), now.checked_add(hour).unwrap().checked_add(hour).unwrap()).unwrap(),
            0
        );
    }

    #[test]
    fn a_projection_needs_both_progress_and_time_behind_it() {
        assert_eq!(projection(0, Some(60)), (None, None));
        assert_eq!(projection(50, None), (None, None));
        assert_eq!(projection(50, Some(0)), (None, None));
        assert_eq!(projection(0, None), (None, None));
    }

    #[test]
    fn a_projection_lands_on_the_quarter_hour() {
        assert_eq!(projection(50, Some(30)), (Some(60), Some(30)));
        assert_eq!(projection(25, Some(20)), (Some(75), Some(60)), "80 and 60 round to the grain");
        assert_eq!(projection(10, Some(1)), (Some(15), Some(15)), "a sliver still reads as a step");
        assert_eq!(projection(100, Some(90)), (Some(90), Some(0)), "finished means nothing left");
        assert_eq!(projection(100, Some(3)), (Some(15), Some(0)));
    }

    #[test]
    fn progress_is_written_read_back_and_held_in_range() {
        let (conn, uid) = db_with_user();
        let id = task(&conn, uid, "the chapter", None);
        assert_eq!(get(&conn, uid, id).unwrap().unwrap().progress, 0);

        update(&conn, uid, id, TaskPatch { progress: Some(40), ..Default::default() }).unwrap();
        let t = get(&conn, uid, id).unwrap().unwrap();
        assert_eq!(t.progress, 40);
        assert_eq!((t.expected_min, t.remaining_min), (None, None), "no minutes to read from yet");

        conn.execute("UPDATE tasks SET actual_min = 40 WHERE id = ?1", [id]).unwrap();
        let t = get(&conn, uid, id).unwrap().unwrap();
        assert_eq!((t.expected_min, t.remaining_min), (Some(105), Some(60)));

        let e = update(&conn, uid, id, TaskPatch { progress: Some(101), ..Default::default() });
        assert!(matches!(e, Err(UpdateError::Invalid(_))), "{e:?}");
        assert_eq!(get(&conn, uid, id).unwrap().unwrap().progress, 40);
    }

    #[test]
    fn finishing_fills_progress_and_reopening_leaves_it_full() {
        let (conn, uid) = db_with_user();
        let parent = task(&conn, uid, "essay", None);
        let step = task(&conn, uid, "draft", Some(parent));
        update(&conn, uid, step, TaskPatch { state: Some("done".into()), ..Default::default() })
            .unwrap();
        assert_eq!(get(&conn, uid, step).unwrap().unwrap().progress, 100);
        assert_eq!(
            get(&conn, uid, parent).unwrap().unwrap().progress,
            100,
            "the parent the last step finished is finished too"
        );

        update(&conn, uid, parent, TaskPatch { state: Some("open".into()), ..Default::default() })
            .unwrap();
        assert_eq!(get(&conn, uid, parent).unwrap().unwrap().progress, 100);
    }

    #[test]
    fn a_task_can_be_created_part_way_through() {
        let (conn, uid) = db_with_user();
        let made = create(
            &conn,
            uid,
            NewTask { title: "half read".into(), progress: Some(50), ..NewTask::default() },
            "manual",
            Actor::User,
        )
        .unwrap();
        assert_eq!(made.progress, 50);
        let refused = create(
            &conn,
            uid,
            NewTask { title: "too far".into(), progress: Some(120), ..NewTask::default() },
            "manual",
            Actor::User,
        );
        assert!(matches!(refused, Err(UpdateError::Invalid(_))), "{refused:?}");
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
