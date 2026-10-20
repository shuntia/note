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
pub const URGENCY: &[&str] = &["low", "normal", "high"];
/// A live task due inside this many hours reads as pressing.
pub const PRESSING_HOURS: i64 = 48;
const DURATION_STEP_MIN: u32 = 5;
const MAX_DURATION_MIN: u32 = 24 * 60;
const MAX_PROGRESS: u32 = 100;
/// The grain every projected figure lands on.
const PROJECTION_STEP_MIN: u32 = 15;
pub const MAX_TITLE_BYTES: usize = 500;
pub const MAX_CATEGORY_BYTES: usize = 100;
const MAX_TEXT_BYTES: usize = 16 * 1024;
const MAX_EXTERNAL_ID_BYTES: usize = 200;
const MAX_URL_BYTES: usize = 2 * 1024;

/// How many tasks Now holds at once — a list short enough to finish.
pub const NOW_CAP: usize = 3;
/// A root is depth 0; no node sits more levels below it than this.
pub const MAX_DEPTH: i64 = 4;
/// Live children one node holds.
pub const MAX_CHILDREN: usize = 200;
/// Live nodes one tree holds, its root included.
pub const MAX_TREE_NODES: usize = 300;

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

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub state: String,
    pub source: String,
    pub notes: String,
    /// A parent's is the sum of its live children's (`duration_source` derived).
    pub duration_min: Option<u32>,
    pub duration_source: String,
    pub parent_id: Option<i64>,
    pub root_id: i64,
    pub depth: i64,
    /// Order among its siblings.
    pub position: i64,
    pub is_now: bool,
    pub updated_at: String,
    /// The node's own deadline, RFC 3339 UTC like `updated_at`.
    pub due_at: Option<String>,
    /// The `YYYY-MM-DD` day when the task is due some time that day rather
    /// than at a set moment; `due_at` is then the end of it.
    pub due_on: Option<String>,
    /// Its own due, else its nearest ancestor's; `due_from` names whose.
    pub effective_due_at: Option<String>,
    pub effective_due_on: Option<String>,
    pub due_from: Option<i64>,
    /// The importer's own id for this task, opaque here and unique per user.
    pub external_id: Option<String>,
    pub url: String,
    pub notify: String,
    /// Free text, held by the root; a child reports its root's.
    pub category: String,
    /// low, normal or high, the node's own.
    pub urgency: String,
    /// The highest of its own urgency and its ancestors'.
    pub effective_urgency: String,
    /// Effectively due inside `PRESSING_HOURS` or already past due. Derived on read.
    pub pressing: bool,
    /// The goal its root belongs to, and its title.
    pub goal_id: Option<i64>,
    pub goal_title: Option<String>,
    /// When this task's next block still to come starts, as a date-time in the
    /// user's own zone. Filled by `stamp_schedule`, which is where the zone is
    /// known; `None` everywhere else.
    pub scheduled_at: Option<String>,
    /// Minutes actually worked on it or anything under it.
    pub actual_min: Option<u32>,
    /// How far along the task is, 0 to 100.
    pub progress: u32,
    /// Where the minutes already worked say the task will land, and what is left
    /// of that. Derived from `progress` and `actual_min`, never stored.
    pub expected_min: Option<u32>,
    pub remaining_min: Option<u32>,
    #[serde(skip)]
    pub(crate) created_at: String,
}

/// A change a write made beyond what it was asked for, so the caller can say so.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Adjusted {
    pub id: i64,
    pub title: String,
    pub field: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<serde_json::Value>,
    pub why: &'static str,
    /// The node the change made room for or was fitted to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by_title: Option<String>,
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

/// A node with its live children; `children` is always present so the client
/// never has to distinguish "no children" from "field missing".
#[derive(Debug, Clone, Serialize)]
pub struct TaskNode {
    #[serde(flatten)]
    pub task: Task,
    pub children: Vec<TaskNode>,
}

impl TaskNode {
    /// The live leaves under this node in tree order; the node itself when it has none.
    pub fn leaves(&self) -> Vec<&Task> {
        if self.children.is_empty() {
            return vec![&self.task];
        }
        self.children.iter().flat_map(TaskNode::leaves).collect()
    }

    pub fn find(&self, id: i64) -> Option<&TaskNode> {
        if self.task.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(id))
    }

    /// Titles from this node down to `id`, both ends included.
    pub fn path_to(&self, id: i64) -> Option<Vec<String>> {
        if self.task.id == id {
            return Some(vec![self.task.title.clone()]);
        }
        self.children.iter().find_map(|c| {
            let mut rest = c.path_to(id)?;
            rest.insert(0, self.task.title.clone());
            Some(rest)
        })
    }

    /// The first unfinished leaf in tree order, else the node itself while it is open.
    pub fn first_open_leaf(&self) -> Option<&Task> {
        self.leaves()
            .into_iter()
            .find(|t| is_open(&t.state))
            .or_else(|| is_open(&self.task.state).then_some(&self.task))
    }

    /// Every node of the subtree, this one first.
    pub fn walk(&self) -> Vec<&Task> {
        let mut out = vec![&self.task];
        for c in &self.children {
            out.extend(c.walk());
        }
        out
    }
}

fn is_open(state: &str) -> bool {
    matches!(state, "open" | "in_progress")
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
    #[serde(default, deserialize_with = "present")]
    pub due_on: Option<Option<String>>,
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
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub goal_id: Option<i64>,
    #[serde(default)]
    pub urgency: Option<String>,
    /// Created under this one, in order; their `parent_id` is ignored.
    #[serde(default)]
    pub children: Vec<NewTask>,
    /// The user's zone, which places the end of a `due_on` day.
    #[serde(skip)]
    pub zone: Option<jiff::tz::TimeZone>,
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
    /// Its place among its live siblings, 0-based, after any parent change.
    pub position: Option<usize>,
    pub is_now: Option<bool>,
    #[serde(default, deserialize_with = "present")]
    pub due_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub due_on: Option<Option<String>>,
    pub url: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub external_id: Option<Option<String>>,
    pub notify: Option<String>,
    pub progress: Option<u32>,
    pub category: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub goal_id: Option<Option<i64>>,
    pub urgency: Option<String>,
    #[serde(skip)]
    pub actor: Actor,
    #[serde(skip)]
    pub zone: Option<jiff::tz::TimeZone>,
}

pub(crate) fn present<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(d).map(Some)
}

fn now() -> String {
    jiff::Timestamp::now().to_string()
}

/// Runs `f` under a savepoint, so a refusal found partway leaves nothing behind.
fn atomically<T>(
    conn: &Connection,
    f: impl FnOnce() -> Result<T, UpdateError>,
) -> Result<T, UpdateError> {
    conn.execute_batch("SAVEPOINT task_write")?;
    match f() {
        Ok(v) => {
            conn.execute_batch("RELEASE task_write")?;
            Ok(v)
        }
        Err(e) => {
            conn.execute_batch("ROLLBACK TO task_write; RELEASE task_write")?;
            Err(e)
        }
    }
}

fn row_to_task(r: &rusqlite::Row) -> rusqlite::Result<Task> {
    let actual_min: Option<u32> = r.get(15)?;
    let progress: u32 = r.get(16)?;
    let (expected_min, remaining_min) = projection(progress, actual_min);
    let state: String = r.get(3)?;
    let effective_due_at: Option<String> = r.get(26)?;
    let pressing = pressing_at(&state, effective_due_at.as_deref(), jiff::Timestamp::now());
    Ok(Task {
        id: r.get(0)?,
        title: r.get(1)?,
        description: r.get(2)?,
        state,
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
        category: r.get(17)?,
        goal_id: r.get(18)?,
        goal_title: r.get(19)?,
        urgency: r.get(20)?,
        created_at: r.get(21)?,
        due_on: r.get(22)?,
        root_id: r.get(23)?,
        depth: r.get(24)?,
        position: r.get(25)?,
        effective_due_at,
        effective_due_on: r.get(27)?,
        due_from: r.get(28)?,
        effective_urgency: r.get(29)?,
        pressing,
        scheduled_at: None,
        actual_min,
        progress,
        expected_min,
        remaining_min,
    })
}

const COLS: &str = "t.id, t.title, t.description, t.state, t.source, t.notes, t.duration_min, \
    t.duration_source, t.parent_id, t.is_now, t.updated_at, t.due_at, \
    t.external_id, t.url, t.notify, t.actual_min, t.progress, \
    COALESCE(r.category, t.category), COALESCE(r.goal_id, t.goal_id), g.title, \
    t.urgency, t.created_at, t.due_on, t.root_id, t.depth, t.position, \
    COALESCE(t.due_at, a1.due_at, a2.due_at, a3.due_at, a4.due_at), \
    CASE WHEN t.due_at IS NOT NULL THEN t.due_on WHEN a1.due_at IS NOT NULL THEN a1.due_on \
         WHEN a2.due_at IS NOT NULL THEN a2.due_on WHEN a3.due_at IS NOT NULL THEN a3.due_on \
         ELSE a4.due_on END, \
    CASE WHEN t.due_at IS NOT NULL THEN t.id WHEN a1.due_at IS NOT NULL THEN a1.id \
         WHEN a2.due_at IS NOT NULL THEN a2.id WHEN a3.due_at IS NOT NULL THEN a3.id \
         WHEN a4.due_at IS NOT NULL THEN a4.id END, \
    CASE WHEN 'high' IN (t.urgency, a1.urgency, a2.urgency, a3.urgency, a4.urgency) THEN 'high' \
         WHEN 'normal' IN (t.urgency, a1.urgency, a2.urgency, a3.urgency, a4.urgency) THEN 'normal' \
         ELSE 'low' END";

/// A node reads its ancestors (at most `MAX_DEPTH` of them) for what it
/// inherits, and its root for the category and goal it reports.
const FROM: &str = "tasks t
    LEFT JOIN tasks a1 ON a1.id = t.parent_id
    LEFT JOIN tasks a2 ON a2.id = a1.parent_id
    LEFT JOIN tasks a3 ON a3.id = a2.parent_id
    LEFT JOIN tasks a4 ON a4.id = a3.parent_id
    LEFT JOIN tasks r ON r.id = t.root_id AND t.parent_id IS NOT NULL
    LEFT JOIN goals g ON g.id = COALESCE(r.goal_id, t.goal_id)";

/// Every node under `?1`, `?1` included, whatever its state.
const SUBTREE: &str = "WITH RECURSIVE sub(id) AS (
    SELECT ?1 UNION ALL SELECT c.id FROM tasks c JOIN sub ON c.parent_id = sub.id)";

fn checked_duration(min: u32) -> Result<u32, UpdateError> {
    if min == 0 || !min.is_multiple_of(DURATION_STEP_MIN) || min > MAX_DURATION_MIN {
        return Err(UpdateError::InvalidDuration(format!(
            "duration_min must be a multiple of {DURATION_STEP_MIN}, from {DURATION_STEP_MIN} to {MAX_DURATION_MIN}"
        )));
    }
    Ok(min)
}

/// A parent with live children holds their progress, each weighted by its
/// duration (a child without one weighs what its siblings average) and a
/// finished child counted full. A finished parent keeps its 100.
const DERIVE_PROGRESS: &str = "
    UPDATE tasks SET progress = COALESCE((
        SELECT CAST(ROUND(SUM(w * p) * 1.0 / SUM(w)) AS INTEGER) FROM (
            SELECT COALESCE(s.duration_min, (
                    SELECT AVG(a.duration_min) FROM tasks a
                    WHERE a.parent_id = s.parent_id AND a.state != 'dropped'
                ), 1) AS w,
                CASE WHEN s.state = 'done' THEN 100 ELSE s.progress END AS p
            FROM tasks s WHERE s.parent_id = tasks.id AND s.state != 'dropped'
        )
    ), progress)
    WHERE id = ?1 AND state != 'done'";

fn checked_progress(value: u32) -> Result<u32, UpdateError> {
    if value > MAX_PROGRESS {
        return Err(UpdateError::Invalid(format!("progress must be 0 to {MAX_PROGRESS}")));
    }
    Ok(value)
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

fn checked_urgency(raw: &str) -> Result<String, UpdateError> {
    if !URGENCY.contains(&raw) {
        return Err(UpdateError::Invalid(format!("urgency must be one of {}", URGENCY.join(", "))));
    }
    Ok(raw.to_owned())
}

pub fn pressing_at(state: &str, due_at: Option<&str>, now: jiff::Timestamp) -> bool {
    if !is_open(state) {
        return false;
    }
    let Some(due) = due_at.and_then(|d| d.parse::<jiff::Timestamp>().ok()) else { return false };
    due < now + jiff::Span::new().hours(PRESSING_HOURS)
}

/// 0 high, 1 pressing, 2 normal, 3 low: the order planning reads.
pub fn urgency_rank(urgency: &str, pressing: bool) -> u8 {
    match (urgency, pressing) {
        ("high", _) => 0,
        (_, true) => 1,
        ("low", false) => 3,
        _ => 2,
    }
}

fn checked_category(raw: &str) -> Result<String, UpdateError> {
    let category = raw.trim();
    if category.len() > MAX_CATEGORY_BYTES {
        return Err(UpdateError::Invalid(format!(
            "category must be at most {MAX_CATEGORY_BYTES} bytes"
        )));
    }
    Ok(category.to_owned())
}

/// A category and a goal belong to the whole tree, so only its root carries
/// them; every other node reads the root's.
fn checked_root_fields(
    parent_id: Option<i64>,
    category: Option<&str>,
    goal_id: Option<i64>,
) -> Result<(), UpdateError> {
    if parent_id.is_none() {
        return Ok(());
    }
    if category.is_some_and(|c| !c.is_empty()) {
        return Err(UpdateError::Invalid(
            "a step carries no category of its own; it reads the one on the task it belongs to"
                .into(),
        ));
    }
    if goal_id.is_some() {
        return Err(UpdateError::Invalid(
            "a step belongs to no goal of its own; the task it belongs to holds one".into(),
        ));
    }
    Ok(())
}

/// A goal a task names must be the caller's own.
fn checked_goal(conn: &Connection, user_id: i64, goal_id: i64) -> Result<(), UpdateError> {
    let mine: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM goals WHERE id = ?1 AND user_id = ?2)",
        (goal_id, user_id),
        |r| r.get(0),
    )?;
    if mine {
        return Ok(());
    }
    Err(UpdateError::Invalid(format!("no goal {goal_id}")))
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

fn one(conn: &Connection, id: i64) -> rusqlite::Result<Option<Task>> {
    conn.query_row(&format!("SELECT {COLS} FROM {FROM} WHERE t.id = ?1"), [id], row_to_task)
        .optional()
}

fn title_of(conn: &Connection, id: i64) -> rusqlite::Result<String> {
    conn.query_row("SELECT title FROM tasks WHERE id = ?1", [id], |r| r.get(0))
}

fn live_children(conn: &Connection, id: i64) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE parent_id = ?1 AND state != 'dropped'",
        [id],
        |r| r.get(0),
    )
}

fn live_in_tree(conn: &Connection, root_id: i64) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE root_id = ?1 AND state != 'dropped'",
        [root_id],
        |r| r.get(0),
    )
}

/// Ids above `id`, its parent first.
fn ancestors(conn: &Connection, id: i64) -> rusqlite::Result<Vec<i64>> {
    let mut out = Vec::new();
    let mut at = id;
    while let Some(p) = conn
        .query_row("SELECT parent_id FROM tasks WHERE id = ?1", [at], |r| r.get::<_, Option<i64>>(0))
        .optional()?
        .flatten()
    {
        out.push(p);
        at = p;
    }
    Ok(out)
}

/// `id` and everything under it, shallowest first.
fn subtree_ids(conn: &Connection, id: i64) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached(&format!(
        "{SUBTREE} SELECT t.id FROM tasks t JOIN sub ON sub.id = t.id ORDER BY t.depth, t.position, t.id"
    ))?;
    let rows = stmt.query_map([id], |r| r.get(0))?;
    rows.collect()
}

/// Ids of the tasks that render in Now, oldest first. A done or dropped task
/// keeps its flag — that is where undo finds its group again — but frees the
/// slot it was holding.
fn now_members(conn: &Connection, user_id: i64) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM tasks
         WHERE user_id = ?1 AND is_now = 1 AND state IN ('open','in_progress')
         ORDER BY id",
    )?;
    let rows = stmt.query_map([user_id], |r| r.get(0))?;
    rows.collect()
}

/// The ancestors and descendants of `id`.
fn relatives(conn: &Connection, id: i64) -> rusqlite::Result<Vec<i64>> {
    let mut out = ancestors(conn, id)?;
    out.extend(subtree_ids(conn, id)?.into_iter().filter(|&d| d != id));
    Ok(out)
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
/// full Now. A relative of `target` gives its place up, so it holds none.
fn check_now_room(
    conn: &Connection,
    user_id: i64,
    actor: Actor,
    target: i64,
) -> Result<(), UpdateError> {
    if actor == Actor::Agent {
        return Ok(());
    }
    let family = relatives(conn, target)?;
    let members: Vec<i64> =
        now_members(conn, user_id)?.into_iter().filter(|m| !family.contains(m)).collect();
    if members.len() < NOW_CAP || members.contains(&target) {
        return Ok(());
    }
    Err(UpdateError::NowFull(format!("Now already holds {NOW_CAP} tasks")))
}

/// Takes `id`'s ancestors and descendants out of Now, now that `id` is in it.
fn take_now_place(conn: &Connection, id: i64, adjusted: &mut Vec<Adjusted>) -> rusqlite::Result<()> {
    let by_title = title_of(conn, id)?;
    for r in relatives(conn, id)? {
        let held: bool = conn.query_row("SELECT is_now FROM tasks WHERE id = ?1", [r], |x| x.get(0))?;
        if !held {
            continue;
        }
        conn.execute("UPDATE tasks SET is_now = 0, updated_at = ?1 WHERE id = ?2", (now(), r))?;
        adjusted.push(Adjusted {
            id: r,
            title: title_of(conn, r)?,
            field: "is_now",
            to: Some(false.into()),
            why: "a task and its part never sit in Now together",
            by: Some(id),
            by_title: Some(by_title.clone()),
        });
    }
    Ok(())
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

/// Finishing or dropping a node does the same to everything live under it.
fn cascade_down(conn: &Connection, id: i64, state: &str) -> rusqlite::Result<()> {
    if state != "done" && state != "dropped" {
        return Ok(());
    }
    conn.execute(
        &format!(
            "{SUBTREE} UPDATE tasks SET state = ?2, updated_at = ?3,
                 completed_at = CASE WHEN ?2 = 'done' THEN COALESCE(completed_at, ?3) END,
                 progress = CASE WHEN ?2 = 'done' THEN 100 ELSE progress END
             WHERE id IN sub AND id != ?1 AND state != 'dropped' AND state != ?2"
        ),
        (id, state, now()),
    )?;
    Ok(())
}

/// A parent is done exactly when every live child is; a done parent with an
/// unfinished child reopens, and an open one with a child under way is under
/// way itself.
fn settle_state(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    let state: String = conn.query_row("SELECT state FROM tasks WHERE id = ?1", [id], |r| r.get(0))?;
    let (total, done, active): (i64, i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(state = 'done'), 0), COALESCE(SUM(state = 'in_progress'), 0)
         FROM tasks WHERE parent_id = ?1 AND state != 'dropped'",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if total == 0 {
        return Ok(());
    }
    let wanted = if done == total {
        "done"
    } else if state == "done" {
        if done > 0 { "in_progress" } else { "open" }
    } else if state == "open" && active > 0 {
        "in_progress"
    } else {
        return Ok(());
    };
    if wanted != state {
        set_state(conn, id, wanted)?;
    }
    Ok(())
}

/// Shares what is left of a sized parent among its unsized children, rounded to
/// whole 5 minutes and never under 5.
fn share(left: u32, ways: u32) -> u32 {
    (((left / ways) + 2) / DURATION_STEP_MIN * DURATION_STEP_MIN).max(DURATION_STEP_MIN)
}

/// A parent's size is the sum of its live children's once any of them has one;
/// until then it keeps its own. The parent that turns derived hands what is left
/// of its own size to the children that have none. Progress follows the same way.
fn rederive(conn: &Connection, id: i64, actor: Actor, adjusted: &mut Vec<Adjusted>) -> rusqlite::Result<()> {
    let (own, source): (Option<u32>, String) = conn.query_row(
        "SELECT duration_min, duration_source FROM tasks WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mut stmt = conn.prepare_cached(
        "SELECT id, title, duration_min FROM tasks
         WHERE parent_id = ?1 AND state != 'dropped' ORDER BY position, id",
    )?;
    let kids: Vec<(i64, String, Option<u32>)> = stmt
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    if kids.is_empty() {
        if source == "derived" {
            let to = if own.is_some() { actor.as_str() } else { "none" };
            conn.execute("UPDATE tasks SET duration_source = ?1 WHERE id = ?2", (to, id))?;
        }
        return Ok(());
    }
    let sized: u32 = kids.iter().filter_map(|k| k.2).sum();
    let bare: Vec<&(i64, String, Option<u32>)> = kids.iter().filter(|k| k.2.is_none()).collect();
    if bare.len() < kids.len() {
        let mut total = sized;
        if let (Some(own), false, false) = (own, source == "derived", bare.is_empty()) {
            let each = share(own.saturating_sub(sized), bare.len() as u32);
            for (kid, title, _) in bare {
                conn.execute(
                    "UPDATE tasks SET duration_min = ?1, duration_source = ?2 WHERE id = ?3",
                    (each, actor.as_str(), kid),
                )?;
                adjusted.push(Adjusted {
                    id: *kid,
                    title: title.clone(),
                    field: "duration_min",
                    to: Some(each.into()),
                    why: "an unsized part shares what is left of its parent's size",
                    by: Some(id),
                    by_title: Some(title_of(conn, id)?),
                });
                total += each;
            }
        }
        conn.execute(
            "UPDATE tasks SET duration_min = ?1, duration_source = 'derived' WHERE id = ?2",
            (total, id),
        )?;
    }
    conn.execute(DERIVE_PROGRESS, [id])?;
    Ok(())
}

/// Settles and rederives `from` and every node above it, nearest first.
fn refresh_up(
    conn: &Connection,
    from: Option<i64>,
    actor: Actor,
    adjusted: &mut Vec<Adjusted>,
) -> rusqlite::Result<()> {
    let Some(first) = from else { return Ok(()) };
    let mut chain = vec![first];
    chain.extend(ancestors(conn, first)?);
    for id in chain {
        settle_state(conn, id)?;
        rederive(conn, id, actor, adjusted)?;
    }
    Ok(())
}

/// Settles and rederives a freshly built subtree, deepest first.
fn refresh_subtree(conn: &Connection, id: i64, actor: Actor, adjusted: &mut Vec<Adjusted>) -> rusqlite::Result<()> {
    for node in subtree_ids(conn, id)?.into_iter().rev() {
        settle_state(conn, node)?;
        rederive(conn, node, actor, adjusted)?;
    }
    Ok(())
}

fn later_than(a: &str, b: &str) -> bool {
    match (a.parse::<jiff::Timestamp>(), b.parse::<jiff::Timestamp>()) {
        (Ok(a), Ok(b)) => a > b,
        _ => false,
    }
}

/// A node's own due never falls after its parent's effective one: `id` and
/// every live node under it are brought back to it, each clamp reported.
fn clamp_dues(conn: &Connection, id: i64, adjusted: &mut Vec<Adjusted>) -> rusqlite::Result<()> {
    for node in subtree_ids(conn, id)? {
        let Some(t) = one(conn, node)? else { continue };
        let (Some(own), Some(parent_id)) = (t.due_at.as_deref(), t.parent_id) else { continue };
        if t.state == "dropped" {
            continue;
        }
        let Some(parent) = one(conn, parent_id)? else { continue };
        let Some(limit) = parent.effective_due_at.as_deref() else { continue };
        if !later_than(own, limit) {
            continue;
        }
        conn.execute(
            "UPDATE tasks SET due_at = ?1, due_on = ?2, updated_at = ?3 WHERE id = ?4",
            (limit, &parent.effective_due_on, now(), node),
        )?;
        let by = parent.due_from;
        adjusted.push(Adjusted {
            id: node,
            title: t.title,
            field: "due",
            to: Some(parent.effective_due_on.clone().unwrap_or_else(|| limit.to_owned()).into()),
            why: "a child is never due after its parent",
            by,
            by_title: by.map(|b| title_of(conn, b)).transpose()?,
        });
    }
    Ok(())
}

fn next_position(conn: &Connection, parent_id: i64) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM tasks WHERE parent_id = ?1",
        [parent_id],
        |r| r.get(0),
    )
}

/// Moves `id` to `index` among its live siblings; dropped ones follow them.
fn place_among(conn: &Connection, id: i64, index: usize) -> rusqlite::Result<()> {
    let Some(parent) = conn.query_row("SELECT parent_id FROM tasks WHERE id = ?1", [id], |r| r.get::<_, Option<i64>>(0))? else {
        return Ok(());
    };
    let mut stmt = conn.prepare_cached(
        "SELECT id, state = 'dropped' FROM tasks WHERE parent_id = ?1 AND id != ?2 ORDER BY position, id",
    )?;
    let sibs: Vec<(i64, bool)> =
        stmt.query_map((parent, id), |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut order: Vec<i64> = sibs.iter().filter(|s| !s.1).map(|s| s.0).collect();
    order.insert(index.min(order.len()), id);
    order.extend(sibs.iter().filter(|s| s.1).map(|s| s.0));
    for (pos, sib) in order.iter().enumerate() {
        conn.execute("UPDATE tasks SET position = ?1 WHERE id = ?2", (pos as i64, sib))?;
    }
    Ok(())
}

/// Everything under `id` in tree order, whatever its state.
fn descendants_in_order(conn: &Connection, id: i64, out: &mut Vec<i64>) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare_cached("SELECT id FROM tasks WHERE parent_id = ?1 ORDER BY position, id")?;
    let kids: Vec<i64> = stmt.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for k in kids {
        out.push(k);
        descendants_in_order(conn, k, out)?;
    }
    Ok(())
}

/// Gives `id` and its subtree their root and depth. Whatever would sit deeper
/// than `MAX_DEPTH` is re-hung beside the node at that depth, in tree order,
/// and a parent so emptied gives up the size it summed.
fn replant(
    conn: &Connection,
    id: i64,
    root_id: i64,
    depth: i64,
    adjusted: &mut Vec<Adjusted>,
) -> rusqlite::Result<()> {
    conn.execute("UPDATE tasks SET root_id = ?1, depth = ?2 WHERE id = ?3", (root_id, depth, id))?;
    if depth < MAX_DEPTH {
        let mut stmt = conn.prepare_cached("SELECT id FROM tasks WHERE parent_id = ?1 ORDER BY position, id")?;
        let kids: Vec<i64> = stmt.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        for k in kids {
            replant(conn, k, root_id, depth + 1, adjusted)?;
        }
        return Ok(());
    }
    let mut below = Vec::new();
    descendants_in_order(conn, id, &mut below)?;
    if below.is_empty() {
        return Ok(());
    }
    let parent: i64 = conn.query_row("SELECT parent_id FROM tasks WHERE id = ?1", [id], |r| r.get(0))?;
    let mut stmt = conn.prepare_cached("SELECT id FROM tasks WHERE parent_id = ?1 ORDER BY position, id")?;
    let sibs: Vec<i64> = stmt.query_map([parent], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let mut order = Vec::new();
    for s in sibs {
        order.push(s);
        if s == id {
            order.extend(below.iter().copied());
        }
    }
    for node in std::iter::once(id).chain(below.iter().copied()) {
        conn.execute(
            "UPDATE tasks SET duration_min = NULL, duration_source = 'none'
             WHERE id = ?1 AND duration_source = 'derived'",
            [node],
        )?;
    }
    for (pos, node) in order.iter().enumerate() {
        conn.execute(
            "UPDATE tasks SET parent_id = ?1, root_id = ?2, depth = ?3, position = ?4 WHERE id = ?5",
            rusqlite::params![parent, root_id, MAX_DEPTH, pos as i64, node],
        )?;
    }
    for node in below {
        adjusted.push(Adjusted {
            id: node,
            title: title_of(conn, node)?,
            field: "depth",
            to: Some(MAX_DEPTH.into()),
            why: "nothing sits more than 4 levels below its root",
            by: Some(id),
            by_title: Some(title_of(conn, id)?),
        });
    }
    Ok(())
}

/// A parent must be the caller's own, sit above the depth cap, have room, and
/// not be the node itself or one of its parts.
fn checked_parent(
    conn: &Connection,
    user_id: i64,
    parent_id: i64,
    child: Option<&Task>,
) -> Result<Task, UpdateError> {
    if child.is_some_and(|c| c.id == parent_id) {
        return Err(UpdateError::InvalidHierarchy("a task cannot be its own step".into()));
    }
    let Some(parent) = get(conn, user_id, parent_id)? else {
        return Err(UpdateError::InvalidHierarchy(format!("no task {parent_id}")));
    };
    let moving = match child {
        Some(c) => {
            if subtree_ids(conn, c.id)?.contains(&parent_id) {
                return Err(UpdateError::InvalidHierarchy(
                    "a task cannot sit under one of its own parts".into(),
                ));
            }
            conn.query_row(
                &format!("{SUBTREE} SELECT COUNT(*) FROM tasks WHERE id IN sub AND state != 'dropped'"),
                [c.id],
                |r| r.get::<_, i64>(0),
            )?
        }
        None => 1,
    };
    if parent.depth >= MAX_DEPTH {
        return Err(UpdateError::InvalidHierarchy(format!(
            "a task sits at most {MAX_DEPTH} levels below its root"
        )));
    }
    if live_children(conn, parent_id)? as usize >= MAX_CHILDREN {
        return Err(UpdateError::InvalidHierarchy(format!(
            "a task holds at most {MAX_CHILDREN} steps"
        )));
    }
    let same_tree = child.is_some_and(|c| c.root_id == parent.root_id);
    if !same_tree && (live_in_tree(conn, parent.root_id)? + moving) as usize > MAX_TREE_NODES {
        return Err(UpdateError::InvalidHierarchy(format!(
            "a tree holds at most {MAX_TREE_NODES} live tasks"
        )));
    }
    Ok(parent)
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
    Ok(create_reported(conn, user_id, new, source, actor)?.node.task)
}

/// A created node with everything built under it, and what the write adjusted.
#[derive(Debug, Serialize)]
pub struct Created {
    #[serde(flatten)]
    pub node: TaskNode,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub adjusted: Vec<Adjusted>,
}

pub fn create_reported(
    conn: &Connection,
    user_id: i64,
    new: NewTask,
    source: &str,
    actor: Actor,
) -> Result<Created, UpdateError> {
    atomically(conn, || {
        let mut adjusted = Vec::new();
        let parent = new.parent_id;
        let id = insert_tree(conn, user_id, new, source, actor, &mut adjusted)?;
        refresh_subtree(conn, id, actor, &mut adjusted)?;
        refresh_up(conn, parent, actor, &mut adjusted)?;
        clamp_dues(conn, id, &mut adjusted)?;
        let is_now: bool = conn.query_row("SELECT is_now FROM tasks WHERE id = ?1", [id], |r| r.get(0))?;
        if is_now {
            take_now_place(conn, id, &mut adjusted)?;
        }
        trim_now(conn, user_id, id)?;
        let node = node(conn, user_id, id)?.expect("row was just created");
        Ok(Created { node, adjusted })
    })
}

/// Inserts `new` and its children, leaving sizes, states and dues to be
/// settled once the whole subtree stands.
fn insert_tree(
    conn: &Connection,
    user_id: i64,
    mut new: NewTask,
    source: &str,
    actor: Actor,
    adjusted: &mut Vec<Adjusted>,
) -> Result<i64, UpdateError> {
    let title = checked_title(&new.title)?;
    let duration = new.duration_min.map(checked_duration).transpose()?;
    let description = new.description.take().unwrap_or_default();
    let notes = new.notes.take().unwrap_or_default();
    checked_text("description", &description)?;
    checked_text("notes", &notes)?;
    let (due_at, due_on) = match crate::due::from_wire(&new.due_at, &new.due_on)?.flatten() {
        Some(due) => {
            let (at, on) = due.columns(new.zone.as_ref())?;
            (Some(at), on)
        }
        None => (None, None),
    };
    let category = new.category.as_deref().map(checked_category).transpose()?;
    let urgency = new.urgency.as_deref().map(checked_urgency).transpose()?;
    checked_root_fields(new.parent_id, category.as_deref(), new.goal_id)?;
    if let Some(goal_id) = new.goal_id {
        checked_goal(conn, user_id, goal_id)?;
    }
    let state = match new.state.as_deref() {
        Some(s) if !STATES.contains(&s) => return Err(UpdateError::InvalidState(s.to_owned())),
        other => other.unwrap_or("open").to_owned(),
    };
    let url = checked_url(new.url.as_deref().unwrap_or_default())?;
    let external_id = new.external_id.as_deref().map(checked_external_id).transpose()?;
    let source_written = checked_source(new.source.as_deref().unwrap_or(source))?;
    let notify = new.notify.as_deref().map(checked_notify).transpose()?;
    let progress = new.progress.map(checked_progress).transpose()?.unwrap_or(0);
    let parent = new.parent_id.map(|p| checked_parent(conn, user_id, p, None)).transpose()?;
    if let Some(id) = &external_id {
        if let Some(held) = external_id_holder(conn, user_id, id, None)? {
            return Err(UpdateError::ExternalIdTaken(format!(
                "external_id {id} already belongs to task {held}"
            )));
        }
    }
    let duration_source = if duration.is_some() { actor.as_str() } else { "none" };
    let (root_id, depth, position) = match &parent {
        Some(p) => (Some(p.root_id), p.depth + 1, next_position(conn, p.id)?),
        None => (None, 0, 0),
    };
    conn.execute(
        "INSERT INTO tasks
            (user_id, title, description, notes, source, state, parent_id, duration_min,
             duration_source, is_now, due_at, url, external_id, notify, created_at, updated_at,
             completed_at, progress, category, goal_id, urgency, due_on, root_id, depth, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, ?11, ?12,
                 COALESCE(?14, 'notify'), ?13, ?13,
                 CASE WHEN ?6 = 'done' THEN ?13 END,
                 CASE WHEN ?6 = 'done' THEN 100 ELSE ?15 END,
                 COALESCE(?16, ''), ?17, ?18, ?19, ?20, ?21, ?22)",
        rusqlite::params![
            user_id,
            &title,
            &description,
            &notes,
            &source_written,
            &state,
            new.parent_id,
            duration,
            duration_source,
            due_at,
            url,
            external_id,
            now(),
            notify,
            progress,
            category,
            new.goal_id,
            urgency.unwrap_or_else(|| "normal".to_string()),
            due_on,
            root_id,
            depth,
            position,
        ],
    )?;
    let id = conn.last_insert_rowid();
    if new.is_now {
        check_now_room(conn, user_id, actor, id)?;
        conn.execute("UPDATE tasks SET is_now = 1 WHERE id = ?1", [id])?;
    }
    let zone = new.zone.clone();
    for mut child in std::mem::take(&mut new.children) {
        child.zone = child.zone.or_else(|| zone.clone());
        let flat = depth >= MAX_DEPTH;
        child.parent_id = Some(if flat { new.parent_id.expect("a node at the cap has a parent") } else { id });
        let child_id = insert_tree(conn, user_id, child, source, actor, adjusted)?;
        if flat {
            adjusted.push(Adjusted {
                id: child_id,
                title: title_of(conn, child_id)?,
                field: "depth",
                to: Some(MAX_DEPTH.into()),
                why: "nothing sits more than 4 levels below its root",
                by: Some(id),
                by_title: Some(title.clone()),
            });
        }
    }
    Ok(id)
}

pub fn get(conn: &Connection, user_id: i64, task_id: i64) -> rusqlite::Result<Option<Task>> {
    conn.query_row(
        &format!("SELECT {COLS} FROM {FROM} WHERE t.id = ?1 AND t.user_id = ?2"),
        (task_id, user_id),
        row_to_task,
    )
    .optional()
}

pub fn list(conn: &Connection, user_id: i64) -> rusqlite::Result<Vec<TaskNode>> {
    trees(conn, user_id, "t.state != 'dropped'")
}

/// The user's open and in-progress roots with their trees.
fn list_open(conn: &Connection, user_id: i64) -> rusqlite::Result<Vec<TaskNode>> {
    trees(conn, user_id, "t.state IN ('open','in_progress')")
}

fn assemble(task: Task, below: &mut std::collections::HashMap<i64, Vec<Task>>) -> TaskNode {
    let children = below
        .remove(&task.id)
        .unwrap_or_default()
        .into_iter()
        .map(|c| assemble(c, below))
        .collect();
    TaskNode { task, children }
}

/// Roots matching `root_filter` (a condition on `t`) in id order, each with its
/// live descendants by position.
fn trees(conn: &Connection, user_id: i64, root_filter: &str) -> rusqlite::Result<Vec<TaskNode>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLS} FROM {FROM}
         WHERE t.user_id = ?1 AND {root_filter} AND t.parent_id IS NULL ORDER BY t.id"
    ))?;
    let roots: Vec<Task> = stmt.query_map([user_id], row_to_task)?.collect::<rusqlite::Result<_>>()?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLS} FROM {FROM}
         WHERE t.root_id IN (SELECT t.id FROM tasks t
                             WHERE t.user_id = ?1 AND {root_filter} AND t.parent_id IS NULL)
           AND t.parent_id IS NOT NULL AND t.state != 'dropped'
         ORDER BY t.position, t.id"
    ))?;
    let mut below: std::collections::HashMap<i64, Vec<Task>> = std::collections::HashMap::new();
    for t in stmt.query_map([user_id], row_to_task)? {
        let t = t?;
        below.entry(t.parent_id.expect("selected as a child")).or_default().push(t);
    }
    Ok(roots.into_iter().map(|t| assemble(t, &mut below)).collect())
}

pub fn node(conn: &Connection, user_id: i64, task_id: i64) -> rusqlite::Result<Option<TaskNode>> {
    let Some(task) = get(conn, user_id, task_id)? else { return Ok(None) };
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLS} FROM {FROM}
         WHERE t.root_id = ?1 AND t.depth > ?2 AND t.state != 'dropped'
         ORDER BY t.position, t.id"
    ))?;
    let mut below: std::collections::HashMap<i64, Vec<Task>> = std::collections::HashMap::new();
    for t in stmt.query_map((task.root_id, task.depth), row_to_task)? {
        let t = t?;
        below.entry(t.parent_id.expect("selected as a child")).or_default().push(t);
    }
    Ok(Some(assemble(task, &mut below)))
}

/// One open task as the next thing to start: the leaf it would begin with, the
/// minutes a round of it is planned for, and why it is here.
#[derive(Debug, Serialize)]
pub struct QueueEntry {
    pub task: TaskNode,
    /// The leaf a round begins with; `None` when the root is itself the leaf.
    pub leaf: Option<Task>,
    /// Titles from the root down to the leaf.
    pub path: Vec<String>,
    pub step: Option<Task>,
    pub planned_min: Option<u32>,
    pub reason: &'static str,
    /// The block a scheduled entry was laid in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<i64>,
}

impl QueueEntry {
    pub(crate) fn new(task: TaskNode, leaf: Option<Task>, reason: &'static str) -> Self {
        let path = leaf
            .as_ref()
            .and_then(|l| task.path_to(l.id))
            .unwrap_or_else(|| vec![task.task.title.clone()]);
        Self {
            planned_min: planned_minutes(&task.task, leaf.as_ref()),
            step: leaf.clone(),
            leaf,
            path,
            task,
            reason,
            event_id: None,
        }
    }
}

/// Orders by what the user is on now, then urgency, then dated before undated and
/// soonest due first, then oldest created, then id.
fn rank_key(
    is_now: bool,
    urgency_rank: u8,
    due: Option<jiff::civil::Date>,
    created: &str,
    id: i64,
) -> (std::cmp::Reverse<bool>, u8, bool, Option<jiff::civil::Date>, String, i64) {
    (std::cmp::Reverse(is_now), urgency_rank, due.is_none(), due, created.to_owned(), id)
}

/// The leaf a round on `start` begins with, `None` when it is the root itself.
fn leaf_under(root: &TaskNode, start: &TaskNode) -> Option<Task> {
    start.first_open_leaf().filter(|l| l.id != root.task.id).cloned()
}

/// The open tree `held` belongs to and the leaf a round of it begins with:
/// `held` itself when it is a leaf, else its first unfinished leaf. `None` when
/// `held` or anything above it is closed or gone.
pub(crate) fn start_point(
    conn: &Connection,
    user_id: i64,
    held: &Task,
    now: jiff::Timestamp,
) -> rusqlite::Result<Option<(TaskNode, Option<Task>)>> {
    if !is_open(&held.state) {
        return Ok(None);
    }
    for a in ancestors(conn, held.id)? {
        if !get(conn, user_id, a)?.is_some_and(|t| is_open(&t.state)) {
            return Ok(None);
        }
    }
    let Some(mut root) = node(conn, user_id, held.root_id)? else { return Ok(None) };
    root.task.pressing = pressing_at(&root.task.state, root.task.effective_due_at.as_deref(), now);
    let leaf = root.find(held.id).and_then(|start| leaf_under(&root, start));
    Ok(Some((root, leaf)))
}

/// The leaf's estimate, else the task's, rounded to whole five minutes.
pub(crate) fn planned_minutes(task: &Task, step: Option<&Task>) -> Option<u32> {
    step.and_then(|s| s.duration_min).or(task.duration_min).map(|m| ((m + 2) / 5 * 5).max(5))
}

/// The user's open roots in the order the planner lays them, each ranked by its
/// most pressing open node.
pub fn queue(
    conn: &Connection,
    user_id: i64,
    tz: &jiff::tz::TimeZone,
    now: jiff::Timestamp,
    limit: usize,
) -> rusqlite::Result<Vec<QueueEntry>> {
    let due_of = |t: &Task| t.effective_due_at.as_deref().and_then(|d| d.parse::<jiff::Timestamp>().ok());
    struct Ranked {
        node: TaskNode,
        now_at: Option<i64>,
        rank: u8,
        due: Option<jiff::Timestamp>,
        high: bool,
        pressing: bool,
    }
    let mut ranked: Vec<Ranked> = list_open(conn, user_id)?
        .into_iter()
        .map(|mut node| {
            node.task.pressing = pressing_at(&node.task.state, node.task.effective_due_at.as_deref(), now);
            let open: Vec<&Task> = node.walk().into_iter().filter(|t| is_open(&t.state)).collect();
            let pressing_of = |t: &Task| pressing_at(&t.state, t.effective_due_at.as_deref(), now);
            let rank = open
                .iter()
                .map(|t| urgency_rank(&t.effective_urgency, pressing_of(t)))
                .min()
                .unwrap_or(2);
            let due = open.iter().filter_map(|t| due_of(t)).min();
            let high = open.iter().any(|t| t.effective_urgency == "high");
            let pressing = open.iter().any(|t| pressing_of(t));
            let now_at = open.iter().find(|t| t.is_now).map(|t| t.id);
            Ranked { node, now_at, rank, due, high, pressing }
        })
        .collect();
    ranked.sort_by_cached_key(|r| {
        rank_key(
            r.now_at.is_some(),
            r.rank,
            r.due.map(|t| t.to_zoned(tz.clone()).date()),
            &r.node.task.created_at,
            r.node.task.id,
        )
    });
    Ok(ranked
        .into_iter()
        .take(limit)
        .map(|r| {
            let start = r.now_at.and_then(|id| r.node.find(id)).unwrap_or(&r.node);
            let leaf = leaf_under(&r.node, start);
            let reason = if r.now_at.is_some() {
                "now"
            } else if r.due.is_some_and(|d| d < now) {
                "overdue"
            } else if r.high {
                "urgent"
            } else if r.pressing {
                "due_soon"
            } else {
                "oldest"
            };
            QueueEntry::new(r.node, leaf, reason)
        })
        .collect())
}

/// The next block each of the user's trees still has waiting — its root's or
/// any node's under it — keyed by the root's id. Blocks are stored as a local
/// date and wall time, so the answer is stamped with the offset the user's
/// clock had then.
fn schedule(
    conn: &Connection,
    user_id: i64,
    tz: &jiff::tz::TimeZone,
    today: jiff::civil::Date,
) -> rusqlite::Result<std::collections::HashMap<i64, String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT t.root_id, p.date, e.wall_time
         FROM events e
         JOIN plans p ON p.id = e.plan_id
         JOIN event_tasks et ON et.event_id = e.id
         JOIN tasks t ON t.id = et.task_id
         WHERE p.user_id = ?1 AND e.status IN ('pending','snoozed') AND p.date >= ?2
         ORDER BY p.date, e.wall_time",
    )?;
    let rows = stmt.query_map((user_id, today.to_string()), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
    })?;
    let mut out = std::collections::HashMap::new();
    for row in rows {
        let (task_id, date, wall) = row?;
        if out.contains_key(&task_id) {
            continue;
        }
        if let Some(at) = local_start(tz, &date, &wall) {
            out.insert(task_id, at);
        }
    }
    Ok(out)
}

fn local_start(tz: &jiff::tz::TimeZone, date: &str, wall: &str) -> Option<String> {
    let at = crate::zone::instant(tz, date.parse().ok()?, wall)?;
    Some(at.to_zoned(tz.clone()).strftime("%Y-%m-%dT%H:%M:%S%:z").to_string())
}

/// Fills `scheduled_at` on each of the roots given.
pub fn stamp_schedule<'a>(
    conn: &Connection,
    user_id: i64,
    tz: &jiff::tz::TimeZone,
    tasks: impl IntoIterator<Item = &'a mut Task>,
) -> rusqlite::Result<()> {
    let today = jiff::Timestamp::now().to_zoned(tz.clone()).date();
    let found = schedule(conn, user_id, tz, today)?;
    for task in tasks {
        task.scheduled_at = found.get(&task.id).cloned();
    }
    Ok(())
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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub title: String,
    pub duration_min: u32,
}

/// Every column of a node's row and of its whole subtree, plus their event
/// links, taken verbatim so a session that fails partway can be undone byte
/// for byte.
#[derive(Debug, Clone)]
pub struct Snapshot {
    task_id: i64,
    columns: Vec<String>,
    rows: Vec<Vec<rusqlite::types::Value>>,
    ids: Vec<i64>,
    links: Vec<(i64, i64)>,
}

pub fn snapshot(conn: &Connection, user_id: i64, task_id: i64) -> rusqlite::Result<Snapshot> {
    let mut stmt = conn.prepare(&format!(
        "{SUBTREE} SELECT * FROM tasks WHERE user_id = ?2 AND id IN sub ORDER BY depth, id"
    ))?;
    let columns: Vec<String> = stmt.column_names().iter().map(|s| (*s).to_owned()).collect();
    let width = columns.len();
    let id_col = columns.iter().position(|c| c == "id").expect("tasks has an id");
    let rows = stmt
        .query_map((task_id, user_id), |r| {
            (0..width).map(|i| r.get(i)).collect::<rusqlite::Result<Vec<_>>>()
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let ids = rows
        .iter()
        .filter_map(|row| match row[id_col] {
            rusqlite::types::Value::Integer(i) => Some(i),
            _ => None,
        })
        .collect();
    let mut stmt = conn.prepare(&format!(
        "{SUBTREE} SELECT event_id, task_id FROM event_tasks WHERE task_id IN sub"
    ))?;
    let links = stmt
        .query_map([task_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Snapshot { task_id, columns, rows, ids, links })
}

/// Puts the snapshot back in one transaction: nodes the session added are gone,
/// nodes it removed are back, and every restored row keeps its original id.
pub fn restore(conn: &Connection, snap: &Snapshot) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("PRAGMA defer_foreign_keys = ON")?;
    let mut gone = subtree_ids(&tx, snap.task_id)?;
    gone.retain(|id| !snap.ids.contains(id));
    for id in &gone {
        tx.execute("UPDATE work_sessions SET task_id = NULL WHERE task_id = ?1", [id])?;
        tx.execute("UPDATE work_sessions SET leaf_id = NULL WHERE leaf_id = ?1", [id])?;
    }
    for id in gone.iter().chain(snap.ids.iter()) {
        tx.execute("DELETE FROM event_tasks WHERE task_id = ?1", [id])?;
        tx.execute("DELETE FROM tasks WHERE id = ?1", [id])?;
    }
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
    let parent: Option<i64> = tx
        .query_row("SELECT parent_id FROM tasks WHERE id = ?1", [snap.task_id], |r| r.get(0))
        .optional()?
        .flatten();
    refresh_up(&tx, parent, Actor::User, &mut Vec::new())?;
    tx.commit()
}

/// Appends `steps` after the children the node already has.
pub fn split(
    conn: &Connection,
    user_id: i64,
    task_id: i64,
    steps: Vec<Step>,
    actor: Actor,
) -> Result<Option<TaskNode>, UpdateError> {
    if steps.is_empty() {
        return Err(UpdateError::InvalidHierarchy("a split needs at least one step".into()));
    }
    let Some(parent) = get(conn, user_id, task_id)? else { return Ok(None) };
    if parent.depth >= MAX_DEPTH {
        return Err(UpdateError::InvalidHierarchy(format!(
            "a task sits at most {MAX_DEPTH} levels below its root"
        )));
    }
    let existing = live_children(conn, task_id)? as usize;
    if existing + steps.len() > MAX_CHILDREN {
        return Err(UpdateError::InvalidHierarchy(format!(
            "a task holds at most {MAX_CHILDREN} steps; it has {existing}"
        )));
    }
    if live_in_tree(conn, parent.root_id)? as usize + steps.len() > MAX_TREE_NODES {
        return Err(UpdateError::InvalidHierarchy(format!(
            "a tree holds at most {MAX_TREE_NODES} live tasks"
        )));
    }
    for s in &steps {
        checked_duration(s.duration_min)?;
    }
    atomically(conn, || {
        let mut adjusted = Vec::new();
        for s in steps {
            insert_tree(
                conn,
                user_id,
                NewTask {
                    title: s.title,
                    duration_min: Some(s.duration_min),
                    parent_id: Some(task_id),
                    source: Some(parent.source.clone()),
                    ..NewTask::default()
                },
                &parent.source,
                actor,
                &mut adjusted,
            )?;
        }
        refresh_up(conn, Some(task_id), actor, &mut adjusted)?;
        conn.execute("UPDATE tasks SET updated_at = ?1 WHERE id = ?2", (now(), task_id))?;
        Ok(node(conn, user_id, task_id)?)
    })
}

/// Removes everything under the node and returns its direct children as they
/// were, so the caller can offer an exact undo.
pub fn flatten(
    conn: &Connection,
    user_id: i64,
    task_id: i64,
) -> Result<Option<(TaskNode, Vec<Task>)>, UpdateError> {
    let Some(before) = node(conn, user_id, task_id)? else { return Ok(None) };
    atomically(conn, || {
        let removed: Vec<Task> = before.children.iter().map(|c| c.task.clone()).collect();
        let mut stmt = conn.prepare("SELECT id FROM tasks WHERE parent_id = ?1")?;
        let kids: Vec<i64> = stmt.query_map([task_id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        for k in kids {
            remove_subtree(conn, k)?;
        }
        refresh_up(conn, Some(task_id), Actor::User, &mut Vec::new())?;
        let n = node(conn, user_id, task_id)?.expect("the node stays");
        Ok(Some((n, removed)))
    })
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
        let made = create_reported(
            conn,
            user_id,
            NewTask { external_id: Some(external_id), ..new },
            "import",
            Actor::User,
        )?;
        return Ok(Upsert::Created(made.node));
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
        due_on: new.due_on,
        url: new.url,
        state,
        zone: new.zone,
        ..Default::default()
    };
    update(conn, user_id, id, &patch)?;
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

/// Removes the task, its whole subtree, and every event link to them. `false`
/// when the task is not this user's.
pub fn delete(conn: &Connection, user_id: i64, task_id: i64) -> rusqlite::Result<bool> {
    let tx = conn.unchecked_transaction()?;
    let gone = delete_within(&tx, user_id, task_id)?;
    tx.commit()?;
    Ok(gone)
}

/// Deletes the rows of `id`'s subtree with their links; sessions that ran on
/// them keep their history without the reference.
fn remove_subtree(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute(&format!("{SUBTREE} DELETE FROM event_tasks WHERE task_id IN sub"), [id])?;
    conn.execute(&format!("{SUBTREE} UPDATE work_sessions SET task_id = NULL WHERE task_id IN sub"), [id])?;
    conn.execute(&format!("{SUBTREE} UPDATE work_sessions SET leaf_id = NULL WHERE leaf_id IN sub"), [id])?;
    conn.execute(&format!("{SUBTREE} DELETE FROM tasks WHERE id IN sub"), [id])?;
    Ok(())
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
    let mut stmt = conn.prepare(&format!(
        "{SUBTREE} SELECT external_id FROM tasks WHERE id IN sub AND external_id IS NOT NULL"
    ))?;
    let buried: Vec<String> = stmt.query_map([task_id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for external_id in buried {
        bury(conn, user_id, &external_id)?;
    }
    remove_subtree(conn, task_id)?;
    refresh_up(conn, task.parent_id, Actor::User, &mut Vec::new())?;
    Ok(true)
}

/// A patched task, plus its parent whenever it has one, since a child's state
/// and progress move the parent's, so the client needs no second round trip.
#[derive(Debug, Serialize)]
pub struct Updated {
    #[serde(flatten)]
    pub task: Task,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<Task>,
    /// Tasks this write pushed out of Now, newest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub demoted_from_now: Vec<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub adjusted: Vec<Adjusted>,
}

/// Returns `Ok(None)` when `task_id` doesn't exist or isn't owned by `user_id`.
pub fn update(
    conn: &Connection,
    user_id: i64,
    task_id: i64,
    patch: &TaskPatch,
) -> Result<Option<Updated>, UpdateError> {
    atomically(conn, || update_within(conn, user_id, task_id, patch))
}

fn update_within(
    conn: &Connection,
    user_id: i64,
    task_id: i64,
    patch: &TaskPatch,
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
    let due = match crate::due::from_wire(&patch.due_at, &patch.due_on)? {
        Some(Some(due)) => Some(Some(due.columns(patch.zone.as_ref())?)),
        other => other.map(|_| None),
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
    let category = patch.category.as_deref().map(checked_category).transpose()?;
    let urgency = patch.urgency.as_deref().map(checked_urgency).transpose()?;
    let Some(before) = get(conn, user_id, task_id)? else { return Ok(None) };
    let moving = patch.parent_id.filter(|p| *p != before.parent_id);
    let parent_id = moving.unwrap_or(before.parent_id);
    if let Some(Some(p)) = moving {
        checked_parent(conn, user_id, p, Some(&before))?;
    }
    checked_root_fields(parent_id, category.as_deref(), patch.goal_id.flatten())?;
    let stays_root = parent_id.is_none() && moving.is_none();
    if let Some(Some(id)) = patch.goal_id {
        checked_goal(conn, user_id, id)?;
    }
    let external_id = external_id.unwrap_or(before.external_id.clone());
    if let Some(id) = &external_id {
        if let Some(held) = external_id_holder(conn, user_id, id, Some(task_id))? {
            return Err(UpdateError::ExternalIdTaken(format!(
                "external_id {id} already belongs to task {held}"
            )));
        }
    }
    let (duration_min, duration_source) = match duration {
        None => (before.duration_min, before.duration_source.clone()),
        Some(None) => (None, "none".to_string()),
        Some(Some(m)) => (Some(m), patch.actor.as_str().to_string()),
    };
    let due_changed = due.is_some();
    let (due_at, due_on) = match due {
        Some(Some((at, on))) => (Some(at), on),
        Some(None) => (None, None),
        None => (before.due_at.clone(), before.due_on.clone()),
    };
    conn.execute(
        "UPDATE tasks SET
            title = COALESCE(?1, title),
            description = COALESCE(?2, description),
            state = COALESCE(?3, state),
            notes = COALESCE(?4, notes),
            duration_min = ?5,
            duration_source = ?6,
            due_at = ?7,
            url = COALESCE(?8, url),
            external_id = ?9,
            notify = COALESCE(?12, notify),
            progress = CASE WHEN COALESCE(?3, state) = 'done' THEN 100
                ELSE COALESCE(?13, progress) END,
            category = CASE WHEN ?17 THEN COALESCE(?14, category) ELSE category END,
            goal_id = CASE WHEN ?17 AND ?18 THEN ?15 ELSE goal_id END,
            urgency = COALESCE(?16, urgency),
            due_on = ?19,
            updated_at = ?10,
            completed_at = CASE WHEN COALESCE(?3, state) = 'done'
                THEN COALESCE(completed_at, ?10) END
         WHERE id = ?11",
        rusqlite::params![
            &title,
            &patch.description,
            &patch.state,
            &patch.notes,
            duration_min,
            duration_source,
            due_at,
            url,
            external_id,
            now(),
            task_id,
            notify,
            progress,
            category,
            patch.goal_id.flatten(),
            urgency,
            stays_root,
            patch.goal_id.is_some(),
            due_on,
        ],
    )?;
    let mut adjusted = Vec::new();
    if let Some(to) = moving {
        move_node(conn, &before, to, category, patch.goal_id, &mut adjusted)?;
    }
    if let Some(index) = patch.position {
        place_among(conn, task_id, index)?;
    }
    match patch.is_now {
        Some(true) => {
            check_now_room(conn, user_id, patch.actor, task_id)?;
            conn.execute("UPDATE tasks SET is_now = 1 WHERE id = ?1", [task_id])?;
            take_now_place(conn, task_id, &mut adjusted)?;
        }
        Some(false) => {
            conn.execute("UPDATE tasks SET is_now = 0 WHERE id = ?1", [task_id])?;
        }
        None if moving.is_some() => clear_now_above(conn, task_id, &mut adjusted)?,
        None => {}
    }
    if let Some(s) = &patch.state {
        cascade_down(conn, task_id, s)?;
    }
    if due_changed || moving.is_some() {
        clamp_dues(conn, task_id, &mut adjusted)?;
    }
    rederive(conn, task_id, patch.actor, &mut adjusted)?;
    refresh_up(conn, parent_id, patch.actor, &mut adjusted)?;
    if moving.is_some() {
        refresh_up(conn, before.parent_id, patch.actor, &mut adjusted)?;
    }
    if let Some(Some(asked)) = duration {
        let (now_min, source): (Option<u32>, String) = conn.query_row(
            "SELECT duration_min, duration_source FROM tasks WHERE id = ?1",
            [task_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if source == "derived" && now_min != Some(asked) {
            adjusted.push(Adjusted {
                id: task_id,
                title: before.title.clone(),
                field: "duration_min",
                to: now_min.map(Into::into),
                why: "a parent's size is the sum of its parts",
                by: None,
                by_title: None,
            });
        }
    }
    let demoted_from_now = trim_now(conn, user_id, task_id)?;
    let parent = match parent_id {
        Some(p) => get(conn, user_id, p)?,
        None => None,
    };
    let task = get(conn, user_id, task_id)?.expect("row was just updated");
    Ok(Some(Updated { task, parent, demoted_from_now, adjusted }))
}

/// Hangs `before` under `to` (a root when `None`) with its whole subtree. A
/// node that becomes a root keeps the category and goal it was reading; a root
/// that becomes a child gives up its own and leaves Now.
fn move_node(
    conn: &Connection,
    before: &Task,
    to: Option<i64>,
    category: Option<String>,
    goal_id: Option<Option<i64>>,
    adjusted: &mut Vec<Adjusted>,
) -> rusqlite::Result<()> {
    let id = before.id;
    match to {
        None => {
            conn.execute(
                "UPDATE tasks SET parent_id = NULL, position = 0, category = ?1, goal_id = ?2 WHERE id = ?3",
                rusqlite::params![category.unwrap_or_else(|| before.category.clone()), goal_id.unwrap_or(before.goal_id), id],
            )?;
            replant(conn, id, id, 0, adjusted)?;
        }
        Some(p) => {
            let position = next_position(conn, p)?;
            conn.execute(
                "UPDATE tasks SET parent_id = ?1, position = ?2, category = '', goal_id = NULL,
                     is_now = CASE WHEN parent_id IS NULL THEN 0 ELSE is_now END
                 WHERE id = ?3",
                (p, position, id),
            )?;
            let (root_id, depth): (i64, i64) =
                conn.query_row("SELECT root_id, depth FROM tasks WHERE id = ?1", [p], |r| Ok((r.get(0)?, r.get(1)?)))?;
            replant(conn, id, root_id, depth + 1, adjusted)?;
            if before.parent_id.is_none() {
                let gave_up = [
                    (!before.category.is_empty(), "category", serde_json::json!(""), "a child reads its root's category"),
                    (before.goal_id.is_some(), "goal_id", serde_json::Value::Null, "a child reads its root's goal"),
                    (before.is_now, "is_now", serde_json::json!(false), "a root that becomes a part leaves Now"),
                ];
                for (lost, field, to, why) in gave_up {
                    if lost {
                        adjusted.push(Adjusted { id, title: before.title.clone(), field, to: Some(to), why, by: None, by_title: None });
                    }
                }
            }
        }
    }
    Ok(())
}

/// After a move, a node of the moved subtree sitting in Now pushes its new
/// ancestors out of it.
fn clear_now_above(conn: &Connection, id: i64, adjusted: &mut Vec<Adjusted>) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(&format!("{SUBTREE} SELECT id FROM tasks WHERE id IN sub AND is_now = 1"))?;
    let held: Vec<i64> = stmt.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    if let Some(&first) = held.first() {
        take_now_place(conn, first, adjusted)?;
    }
    Ok(())
}

/// Set once `rederive_all` has run on this database.
pub const TREE_DERIVED_SETTING: &str = "tree_derived";

/// Recomputes every parent's size and progress, deepest first; for databases
/// whose parents were marked derived before anything summed them.
pub fn rederive_all(conn: &Connection) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    let mut stmt = tx.prepare(
        "SELECT id FROM tasks t WHERE EXISTS (SELECT 1 FROM tasks c WHERE c.parent_id = t.id)
         ORDER BY depth DESC, id",
    )?;
    let parents: Vec<i64> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for id in parents {
        rederive(&tx, id, Actor::Agent, &mut Vec::new())?;
    }
    tx.commit()
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
    fn list_hangs_each_undropped_step_under_its_task_in_id_order() {
        let (conn, uid) = db_with_user();
        let other = crate::auth::create_user(&conn, "aitest", "pw", false).unwrap();
        let a = task(&conn, uid, "a", None);
        let b = task(&conn, uid, "b", None);
        let b1 = task(&conn, uid, "b1", Some(b));
        let a1 = task(&conn, uid, "a1", Some(a));
        let a2 = task(&conn, uid, "a2", Some(a));
        let c = task(&conn, uid, "c", None);
        task(&conn, uid, "c1", Some(c));
        let a3 = task(&conn, uid, "a3", Some(a));
        let d = task(&conn, uid, "d", None);
        let d1 = task(&conn, uid, "d1", Some(d));
        task(&conn, other, "elsewhere", None);
        conn.execute("UPDATE tasks SET state = 'dropped' WHERE id IN (?1, ?2)", [a2, c]).unwrap();
        conn.execute("UPDATE tasks SET state = 'done' WHERE id = ?1", [d]).unwrap();
        let shape = |nodes: Vec<TaskNode>| -> Vec<(i64, Vec<i64>)> {
            nodes.into_iter().map(|n| (n.task.id, n.children.iter().map(|c| c.task.id).collect())).collect()
        };
        assert_eq!(
            shape(list(&conn, uid).unwrap()),
            vec![(a, vec![a1, a3]), (b, vec![b1]), (d, vec![d1])]
        );
        assert_eq!(shape(list_open(&conn, uid).unwrap()), vec![(a, vec![a1, a3]), (b, vec![b1])]);
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
            &TaskPatch {
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

        let step = before.children[0].task.id;
        delete(&conn, uid, step).unwrap();
        update(
            &conn,
            uid,
            id,
            &TaskPatch {
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
        let ids = |n: &TaskNode| n.children.iter().map(|c| c.task.id).collect::<Vec<_>>();
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
        update(&conn, uid, id, &TaskPatch { state: Some("done".into()), ..Default::default() })
            .unwrap();
        let first = completed(id).expect("done carries a completion time");
        update(&conn, uid, id, &TaskPatch { notes: Some("later".into()), ..Default::default() })
            .unwrap();
        assert_eq!(completed(id).as_deref(), Some(first.as_str()), "an unrelated write keeps it");
        update(&conn, uid, id, &TaskPatch { state: Some("open".into()), ..Default::default() })
            .unwrap();
        assert!(completed(id).is_none(), "reopening clears it");
    }

    #[test]
    fn done_between_counts_by_completion_time() {
        let (conn, uid) = db_with_user();
        let id = task(&conn, uid, "write it up", None);
        update(&conn, uid, id, &TaskPatch { state: Some("done".into()), ..Default::default() })
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

        update(&conn, uid, id, &TaskPatch { progress: Some(40), ..Default::default() }).unwrap();
        let t = get(&conn, uid, id).unwrap().unwrap();
        assert_eq!(t.progress, 40);
        assert_eq!((t.expected_min, t.remaining_min), (None, None), "no minutes to read from yet");

        conn.execute("UPDATE tasks SET actual_min = 40 WHERE id = ?1", [id]).unwrap();
        let t = get(&conn, uid, id).unwrap().unwrap();
        assert_eq!((t.expected_min, t.remaining_min), (Some(105), Some(60)));

        let e = update(&conn, uid, id, &TaskPatch { progress: Some(101), ..Default::default() });
        assert!(matches!(e, Err(UpdateError::Invalid(_))), "{e:?}");
        assert_eq!(get(&conn, uid, id).unwrap().unwrap().progress, 40);
    }

    #[test]
    fn finishing_fills_progress_and_reopening_leaves_it_full() {
        let (conn, uid) = db_with_user();
        let parent = task(&conn, uid, "essay", None);
        let step = task(&conn, uid, "draft", Some(parent));
        update(&conn, uid, step, &TaskPatch { state: Some("done".into()), ..Default::default() })
            .unwrap();
        assert_eq!(get(&conn, uid, step).unwrap().unwrap().progress, 100);
        assert_eq!(
            get(&conn, uid, parent).unwrap().unwrap().progress,
            100,
            "the parent the last step finished is finished too"
        );

        update(&conn, uid, parent, &TaskPatch { state: Some("open".into()), ..Default::default() })
            .unwrap();
        assert_eq!(get(&conn, uid, parent).unwrap().unwrap().progress, 100);
    }

    #[test]
    fn a_parent_holds_the_progress_of_its_steps_weighted_by_length() {
        let (conn, uid) = db_with_user();
        let parent = task(&conn, uid, "film analysis", None);
        let step = |title: &str, minutes: Option<u32>| {
            create(
                &conn,
                uid,
                NewTask {
                    title: title.into(),
                    parent_id: Some(parent),
                    duration_min: minutes,
                    ..NewTask::default()
                },
                "manual",
                Actor::User,
            )
            .unwrap()
            .id
        };
        let watch = step("watch", Some(60));
        let notes = step("notes", Some(30));
        let write = step("write", None);
        let progress = |id| get(&conn, uid, id).unwrap().unwrap().progress;
        assert_eq!(progress(parent), 0);

        let u = update(&conn, uid, watch, &TaskPatch { state: Some("done".into()), ..Default::default() })
            .unwrap()
            .unwrap();
        assert_eq!(progress(parent), 44, "60 of 60 + 30 + an unsized step weighing their mean, 45");
        assert_eq!(u.parent.map(|p| p.progress), Some(44), "the write hands the parent back");

        update(&conn, uid, notes, &TaskPatch { progress: Some(50), ..Default::default() }).unwrap();
        assert_eq!(progress(parent), 56);

        update(&conn, uid, parent, &TaskPatch { progress: Some(5), ..Default::default() }).unwrap();
        assert_eq!(progress(parent), 56, "a parent with steps is not set by hand");

        update(&conn, uid, write, &TaskPatch { state: Some("dropped".into()), ..Default::default() })
            .unwrap();
        assert_eq!(progress(parent), 83, "a dropped step weighs nothing");

        delete(&conn, uid, notes).unwrap();
        assert_eq!(progress(parent), 100);
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
    fn a_step_reads_its_parents_category_and_carries_none_of_its_own() {
        let (conn, uid) = db_with_user();
        let parent = create(
            &conn,
            uid,
            NewTask { title: "essay".into(), category: Some("  Biology  ".into()), ..Default::default() },
            "manual",
            Actor::User,
        )
        .unwrap();
        assert_eq!(parent.category, "Biology");
        let step = task(&conn, uid, "draft", Some(parent.id));
        assert_eq!(get(&conn, uid, step).unwrap().unwrap().category, "Biology");

        let refused = update(
            &conn,
            uid,
            step,
            &TaskPatch { category: Some("History".into()), ..Default::default() },
        );
        assert!(matches!(refused, Err(UpdateError::Invalid(_))), "{refused:?}");

        update(&conn, uid, parent.id, &TaskPatch { category: Some("Chemistry".into()), ..Default::default() })
            .unwrap();
        assert_eq!(get(&conn, uid, step).unwrap().unwrap().category, "Chemistry");
    }

    #[test]
    fn a_goal_is_the_users_own_and_never_a_steps() {
        let (conn, uid) = db_with_user();
        let goal = crate::goals::create(
            &conn,
            uid,
            crate::goals::NewGoal { title: "apply".into(), ..Default::default() },
        )
        .unwrap();
        let id = create(
            &conn,
            uid,
            NewTask { title: "essay".into(), goal_id: Some(goal.id), ..Default::default() },
            "manual",
            Actor::User,
        )
        .unwrap()
        .id;
        let t = get(&conn, uid, id).unwrap().unwrap();
        assert_eq!(t.goal_id, Some(goal.id));
        assert_eq!(t.goal_title.as_deref(), Some("apply"));

        let step = task(&conn, uid, "draft", Some(id));
        let refused =
            update(&conn, uid, step, &TaskPatch { goal_id: Some(Some(goal.id)), ..Default::default() });
        assert!(matches!(refused, Err(UpdateError::Invalid(_))), "{refused:?}");

        let bo = crate::auth::create_user(&conn, "bo", "pw", false).unwrap();
        let theirs = crate::goals::create(
            &conn,
            bo,
            crate::goals::NewGoal { title: "theirs".into(), ..Default::default() },
        )
        .unwrap();
        let refused =
            update(&conn, uid, id, &TaskPatch { goal_id: Some(Some(theirs.id)), ..Default::default() });
        assert!(matches!(refused, Err(UpdateError::Invalid(_))), "{refused:?}");

        update(&conn, uid, id, &TaskPatch { goal_id: Some(None), ..Default::default() }).unwrap();
        assert!(get(&conn, uid, id).unwrap().unwrap().goal_id.is_none());
    }

    #[test]
    fn scheduled_at_reads_the_next_block_still_waiting() {
        let (conn, uid) = db_with_user();
        let id = task(&conn, uid, "essay", None);
        let tz = jiff::tz::TimeZone::UTC;
        let today = jiff::Timestamp::now().to_zoned(tz.clone()).date();
        let stamped = |id: i64| -> Option<String> {
            let mut t = get(&conn, uid, id).unwrap().unwrap();
            stamp_schedule(&conn, uid, &tz, std::iter::once(&mut t)).unwrap();
            t.scheduled_at
        };
        assert!(stamped(id).is_none(), "a task nobody has planned has no hour yet");

        let block = |date: jiff::civil::Date, wall: &str, status: &str| {
            conn.execute(
                "INSERT OR IGNORE INTO plans (user_id, date, created_at) VALUES (?1, ?2, 'x')",
                (uid, date.to_string()),
            )
            .unwrap();
            conn.execute(
                "INSERT INTO events (plan_id, kind, wall_time, alert, end_wall_time, status)
                 SELECT id, 'essay', ?2, 0, '10:00', ?3 FROM plans
                 WHERE user_id = ?1 AND date = ?4",
                rusqlite::params![uid, wall, status, date.to_string()],
            )
            .unwrap();
            let event = conn.last_insert_rowid();
            conn.execute("INSERT INTO event_tasks (event_id, task_id) VALUES (?1, ?2)", (event, id))
                .unwrap();
        };
        let tomorrow = today.tomorrow().unwrap();
        block(today.yesterday().unwrap(), "08:00", "pending");
        block(tomorrow, "09:15", "pending");
        assert_eq!(
            stamped(id).as_deref(),
            Some(format!("{tomorrow}T09:15:00+00:00").as_str()),
            "yesterday's block is behind us"
        );

        block(today, "18:15", "dropped");
        assert_eq!(
            stamped(id).as_deref(),
            Some(format!("{tomorrow}T09:15:00+00:00").as_str()),
            "a block the user dropped is not waiting"
        );
        block(today, "18:45", "snoozed");
        assert_eq!(stamped(id).as_deref(), Some(format!("{today}T18:45:00+00:00").as_str()));
    }

    #[test]
    fn a_steps_block_is_the_tasks_own_hour() {
        let (conn, uid) = db_with_user();
        let parent = task(&conn, uid, "essay", None);
        let step = task(&conn, uid, "draft", Some(parent));
        let tz = jiff::tz::TimeZone::UTC;
        let today = jiff::Timestamp::now().to_zoned(tz.clone()).date();
        conn.execute(
            "INSERT INTO plans (user_id, date, created_at) VALUES (?1, ?2, 'x')",
            (uid, today.to_string()),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time, alert, end_wall_time)
             VALUES (1, 'draft', '14:00', 0, '15:00')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO event_tasks (event_id, task_id) VALUES (?1, ?2)",
            (conn.last_insert_rowid(), step),
        )
        .unwrap();

        let mut t = get(&conn, uid, parent).unwrap().unwrap();
        stamp_schedule(&conn, uid, &tz, std::iter::once(&mut t)).unwrap();
        assert_eq!(t.scheduled_at.as_deref(), Some(format!("{today}T14:00:00+00:00").as_str()));
    }

    #[test]
    fn delete_ignores_another_users_task() {
        let (conn, uid) = db_with_user();
        let bo = crate::auth::create_user(&conn, "bo", "pw", false).unwrap();
        let theirs = task(&conn, bo, "theirs", None);
        assert!(!delete(&conn, uid, theirs).unwrap());
        assert!(get(&conn, bo, theirs).unwrap().is_some());
    }

    #[test]
    fn urgency_defaults_to_normal_and_round_trips() {
        let (conn, uid) = db_with_user();
        let t = create(
            &conn,
            uid,
            NewTask { title: "read".into(), ..Default::default() },
            "manual",
            Actor::User,
        )
        .unwrap();
        assert_eq!(t.urgency, "normal");
        let t = create(
            &conn,
            uid,
            NewTask { title: "exam".into(), urgency: Some("high".into()), ..Default::default() },
            "manual",
            Actor::User,
        )
        .unwrap();
        assert_eq!(t.urgency, "high");
        let up = update(&conn, uid, t.id, &TaskPatch { urgency: Some("low".into()), ..Default::default() })
            .unwrap()
            .unwrap();
        assert_eq!(up.task.urgency, "low");
        let err = update(&conn, uid, t.id, &TaskPatch { urgency: Some("asap".into()), ..Default::default() })
            .unwrap_err();
        assert!(matches!(err, UpdateError::Invalid(_)), "{err:?}");
    }

    #[test]
    fn a_step_carries_its_own_urgency_and_reads_the_highest_above_it() {
        let (conn, uid) = db_with_user();
        let parent = create(
            &conn,
            uid,
            NewTask { title: "essay".into(), urgency: Some("high".into()), ..Default::default() },
            "manual",
            Actor::User,
        )
        .unwrap();
        let step = task(&conn, uid, "outline", Some(parent.id));
        let step = get(&conn, uid, step).unwrap().unwrap();
        assert_eq!((step.urgency.as_str(), step.effective_urgency.as_str()), ("normal", "high"));
        let low = create(
            &conn,
            uid,
            NewTask {
                title: "draft".into(),
                parent_id: Some(parent.id),
                urgency: Some("low".into()),
                ..Default::default()
            },
            "manual",
            Actor::User,
        )
        .unwrap();
        assert_eq!((low.urgency.as_str(), low.effective_urgency.as_str()), ("low", "high"));
        update(&conn, uid, parent.id, &TaskPatch { urgency: Some("low".into()), ..Default::default() }).unwrap();
        assert_eq!(get(&conn, uid, step.id).unwrap().unwrap().effective_urgency, "normal");
        let promoted = update(&conn, uid, low.id, &TaskPatch { parent_id: Some(None), ..Default::default() })
            .unwrap()
            .unwrap();
        assert_eq!(promoted.task.urgency, "low", "a promoted step keeps its own urgency");
    }

    #[test]
    fn pressing_flips_at_forty_eight_hours_and_for_overdue() {
        let now: jiff::Timestamp = "2026-09-24T12:00:00Z".parse().unwrap();
        assert!(!pressing_at("open", None, now));
        assert!(pressing_at("open", Some("2026-09-24T11:00:00Z"), now), "overdue is pressing");
        assert!(pressing_at("in_progress", Some("2026-09-26T11:59:59Z"), now), "inside 48h");
        assert!(!pressing_at("open", Some("2026-09-26T12:00:01Z"), now), "past 48h");
        assert!(!pressing_at("open", Some("not a time"), now));
        assert!(!pressing_at("done", Some("2026-09-24T11:00:00Z"), now), "a done task is never pressing");
        assert!(!pressing_at("dropped", Some("2026-09-24T11:00:00Z"), now));
    }

    fn set(conn: &Connection, id: i64, due_at: Option<&str>, urgency: &str, is_now: bool) {
        conn.execute(
            "UPDATE tasks SET due_at = ?1, urgency = ?2, is_now = ?3 WHERE id = ?4",
            rusqlite::params![due_at, urgency, is_now, id],
        )
        .unwrap();
    }

    #[test]
    fn queue_orders_now_then_urgent_then_due_then_oldest() {
        let (conn, uid) = db_with_user();
        let now: jiff::Timestamp = "2026-09-25T20:00:00Z".parse().unwrap();
        let oldest = task(&conn, uid, "oldest", None);
        let soon = task(&conn, uid, "soon", None);
        let over = task(&conn, uid, "over", None);
        let urgent = task(&conn, uid, "urgent", None);
        let current = task(&conn, uid, "now", None);
        let done = task(&conn, uid, "done", None);
        set(&conn, soon, Some("2026-09-26T09:00:00Z"), "normal", false);
        set(&conn, over, Some("2026-09-24T09:00:00Z"), "normal", false);
        set(&conn, urgent, None, "high", false);
        set(&conn, current, None, "low", true);
        conn.execute("UPDATE tasks SET state = 'done' WHERE id = ?1", [done]).unwrap();

        let q = queue(&conn, uid, &jiff::tz::TimeZone::UTC, now, 10).unwrap();
        let ids: Vec<i64> = q.iter().map(|e| e.task.task.id).collect();
        assert_eq!(ids, [current, urgent, over, soon, oldest]);
        let reasons: Vec<&str> = q.iter().map(|e| e.reason).collect();
        assert_eq!(reasons, ["now", "urgent", "overdue", "due_soon", "oldest"]);
        assert_eq!(queue(&conn, uid, &jiff::tz::TimeZone::UTC, now, 2).unwrap().len(), 2);
    }

    #[test]
    fn queue_plans_the_open_step_before_the_whole_task() {
        let (conn, uid) = db_with_user();
        let now: jiff::Timestamp = "2026-09-25T20:00:00Z".parse().unwrap();
        let stepped = task(&conn, uid, "stepped", None);
        let first = task(&conn, uid, "first", Some(stepped));
        let second = task(&conn, uid, "second", Some(stepped));
        let whole = task(&conn, uid, "whole", None);
        let bare = task(&conn, uid, "bare", None);
        for (id, min) in [(stepped, 50), (first, 10), (second, 20), (whole, 45)] {
            conn.execute("UPDATE tasks SET duration_min = ?1 WHERE id = ?2", [min, id]).unwrap();
        }
        conn.execute("UPDATE tasks SET state = 'done' WHERE id = ?1", [first]).unwrap();

        let q = queue(&conn, uid, &jiff::tz::TimeZone::UTC, now, 10).unwrap();
        let entry = |id: i64| q.iter().find(|e| e.task.task.id == id).unwrap();
        assert_eq!(entry(stepped).step.as_ref().map(|s| s.id), Some(second));
        assert_eq!(entry(stepped).planned_min, Some(20));
        assert!(entry(whole).step.is_none());
        assert_eq!(entry(whole).planned_min, Some(45));
        assert_eq!(entry(bare).planned_min, None);
    }

    #[test]
    fn queue_lays_a_dated_task_before_an_undated_one_of_the_same_rank() {
        let (conn, uid) = db_with_user();
        let now: jiff::Timestamp = "2026-09-20T09:00:00Z".parse().unwrap();
        let undated = task(&conn, uid, "tidy the desk", None);
        let dated = task(&conn, uid, "hand in the form", None);
        set(&conn, dated, Some("2026-09-25T09:00:00Z"), "normal", false);
        let q = queue(&conn, uid, &jiff::tz::TimeZone::UTC, now, 10).unwrap();
        let ids: Vec<i64> = q.iter().map(|e| e.task.task.id).collect();
        assert_eq!(ids, [dated, undated]);
        assert_eq!(q[0].reason, "oldest");
    }

    fn sized(conn: &Connection, uid: i64, title: &str, parent: Option<i64>, minutes: Option<u32>) -> i64 {
        create(
            conn,
            uid,
            NewTask { title: title.into(), parent_id: parent, duration_min: minutes, ..NewTask::default() },
            "manual",
            Actor::User,
        )
        .unwrap()
        .id
    }

    fn due_day(conn: &Connection, uid: i64, id: i64, day: &str) -> Updated {
        update(
            conn,
            uid,
            id,
            &TaskPatch { due_on: Some(Some(day.into())), zone: Some(jiff::tz::TimeZone::UTC), ..Default::default() },
        )
        .unwrap()
        .unwrap()
    }

    fn state_of(conn: &Connection, uid: i64, id: i64) -> String {
        get(conn, uid, id).unwrap().unwrap().state
    }

    fn put_state(conn: &Connection, uid: i64, id: i64, state: &str) -> Updated {
        update(conn, uid, id, &TaskPatch { state: Some(state.into()), ..Default::default() }).unwrap().unwrap()
    }

    #[test]
    fn a_node_inherits_the_nearest_due_above_it() {
        let (conn, uid) = db_with_user();
        let course = task(&conn, uid, "course", None);
        let unit = task(&conn, uid, "unit", Some(course));
        let lesson = task(&conn, uid, "lesson", Some(unit));
        let part = task(&conn, uid, "part", Some(lesson));
        due_day(&conn, uid, course, "2026-12-18");
        let t = get(&conn, uid, part).unwrap().unwrap();
        assert!(t.due_on.is_none());
        assert_eq!((t.effective_due_on.as_deref(), t.due_from), (Some("2026-12-18"), Some(course)));
        due_day(&conn, uid, unit, "2026-11-06");
        let t = get(&conn, uid, part).unwrap().unwrap();
        assert_eq!((t.effective_due_on.as_deref(), t.due_from), (Some("2026-11-06"), Some(unit)));
        assert_eq!(t.depth, 3);
        assert_eq!(t.root_id, course);
        assert!(get(&conn, uid, course).unwrap().unwrap().due_on.as_deref() == Some("2026-12-18"), "a parent never takes a child's due");
    }

    #[test]
    fn a_child_due_after_its_parent_is_clamped_and_reported() {
        let (conn, uid) = db_with_user();
        let unit = task(&conn, uid, "Unit 1", None);
        due_day(&conn, uid, unit, "2026-11-06");
        let made = create_reported(
            &conn,
            uid,
            NewTask {
                title: "Lesson 3".into(),
                parent_id: Some(unit),
                due_on: Some(Some("2026-11-20".into())),
                zone: Some(jiff::tz::TimeZone::UTC),
                ..NewTask::default()
            },
            "manual",
            Actor::User,
        )
        .unwrap();
        assert_eq!(made.node.task.due_on.as_deref(), Some("2026-11-06"));
        assert_eq!(made.adjusted.len(), 1);
        let a = &made.adjusted[0];
        assert_eq!((a.field, a.by, a.by_title.as_deref()), ("due", Some(unit), Some("Unit 1")));
        assert_eq!(a.to, Some("2026-11-06".into()));

        let lesson = made.node.task.id;
        let up = due_day(&conn, uid, lesson, "2026-11-30");
        assert_eq!(up.task.due_on.as_deref(), Some("2026-11-06"), "a later due on update is clamped too");
        assert_eq!(up.adjusted.len(), 1);
        let up = due_day(&conn, uid, lesson, "2026-11-03");
        assert!(up.adjusted.is_empty(), "an earlier due stands");
        assert_eq!(up.task.due_on.as_deref(), Some("2026-11-03"));
    }

    #[test]
    fn a_parent_due_moving_earlier_clamps_every_later_descendant() {
        let (conn, uid) = db_with_user();
        let course = task(&conn, uid, "course", None);
        let unit = task(&conn, uid, "unit", Some(course));
        let lesson = task(&conn, uid, "lesson", Some(unit));
        let early = task(&conn, uid, "early", Some(unit));
        due_day(&conn, uid, course, "2026-12-18");
        due_day(&conn, uid, unit, "2026-12-01");
        due_day(&conn, uid, lesson, "2026-11-20");
        due_day(&conn, uid, early, "2026-11-02");
        let moved = due_day(&conn, uid, course, "2026-11-10");
        let mut clamped: Vec<(i64, Option<i64>)> = moved.adjusted.iter().map(|a| (a.id, a.by)).collect();
        clamped.sort_unstable();
        assert_eq!(clamped, [(unit, Some(course)), (lesson, Some(unit))]);
        assert_eq!(get(&conn, uid, lesson).unwrap().unwrap().due_on.as_deref(), Some("2026-11-10"));
        assert_eq!(get(&conn, uid, early).unwrap().unwrap().due_on.as_deref(), Some("2026-11-02"));
    }

    #[test]
    fn sizes_and_progress_derive_up_three_levels() {
        let (conn, uid) = db_with_user();
        let course = task(&conn, uid, "course", None);
        let unit = task(&conn, uid, "unit", Some(course));
        let lesson = task(&conn, uid, "lesson", Some(unit));
        let a = sized(&conn, uid, "a", Some(lesson), Some(20));
        let b = sized(&conn, uid, "b", Some(lesson), Some(40));
        let other = sized(&conn, uid, "other", Some(unit), Some(30));
        let dur = |id| get(&conn, uid, id).unwrap().unwrap();
        assert_eq!((dur(lesson).duration_min, dur(lesson).duration_source.as_str()), (Some(60), "derived"));
        assert_eq!(dur(unit).duration_min, Some(90));
        assert_eq!(dur(course).duration_min, Some(90));

        put_state(&conn, uid, a, "done");
        assert_eq!(dur(lesson).progress, 33);
        assert_eq!(dur(unit).progress, 22, "60 min at 33 and 30 min at 0");
        assert_eq!(dur(course).progress, 22);

        update(&conn, uid, course, &TaskPatch { duration_min: Some(Some(15)), ..Default::default() }).unwrap();
        assert_eq!(dur(course).duration_min, Some(90), "a parent's own size gives way to its parts'");
        update(&conn, uid, b, &TaskPatch { state: Some("dropped".into()), ..Default::default() }).unwrap();
        assert_eq!(dur(unit).duration_min, Some(50));
        delete(&conn, uid, other).unwrap();
        assert_eq!(dur(course).duration_min, Some(20));
        assert_eq!(state_of(&conn, uid, course), "done", "the last open leaf gone, everything above it is done");
    }

    #[test]
    fn a_sized_parent_shares_what_is_left_among_its_unsized_parts() {
        let (conn, uid) = db_with_user();
        let essay = sized(&conn, uid, "essay", None, Some(90));
        let outline = sized(&conn, uid, "outline", Some(essay), None);
        let draft = sized(&conn, uid, "draft", Some(essay), None);
        assert_eq!(get(&conn, uid, essay).unwrap().unwrap().duration_min, Some(90), "all unsized: the parent keeps its own");
        let up = update(&conn, uid, outline, &TaskPatch { duration_min: Some(Some(20)), ..Default::default() })
            .unwrap()
            .unwrap();
        assert_eq!(get(&conn, uid, draft).unwrap().unwrap().duration_min, Some(70));
        assert_eq!(up.adjusted.iter().map(|a| (a.id, a.field)).collect::<Vec<_>>(), [(draft, "duration_min")]);
        let parent = get(&conn, uid, essay).unwrap().unwrap();
        assert_eq!((parent.duration_min, parent.duration_source.as_str()), (Some(90), "derived"));
        let late = sized(&conn, uid, "proof", Some(essay), None);
        assert_eq!(get(&conn, uid, late).unwrap().unwrap().duration_min, None, "a derived parent has nothing left to share");
    }

    #[test]
    fn states_cascade_down_and_settle_up_the_tree() {
        let (conn, uid) = db_with_user();
        let course = task(&conn, uid, "course", None);
        let unit = task(&conn, uid, "unit", Some(course));
        let lesson = task(&conn, uid, "lesson", Some(unit));
        let a = task(&conn, uid, "a", Some(lesson));
        let b = task(&conn, uid, "b", Some(lesson));
        let other = task(&conn, uid, "other", Some(unit));

        put_state(&conn, uid, a, "in_progress");
        for id in [lesson, unit, course] {
            assert_eq!(state_of(&conn, uid, id), "in_progress", "a part under way lifts {id}");
        }
        put_state(&conn, uid, lesson, "done");
        assert_eq!((state_of(&conn, uid, a), state_of(&conn, uid, b)), ("done".into(), "done".into()));
        assert_eq!(state_of(&conn, uid, unit), "in_progress");
        put_state(&conn, uid, other, "done");
        assert_eq!(state_of(&conn, uid, unit), "done");
        assert_eq!(state_of(&conn, uid, course), "done");

        put_state(&conn, uid, b, "open");
        assert_eq!(state_of(&conn, uid, lesson), "in_progress", "a sibling is done");
        assert_eq!(state_of(&conn, uid, unit), "in_progress");
        assert_eq!(state_of(&conn, uid, course), "open", "nothing under the course is done any more");

        put_state(&conn, uid, lesson, "dropped");
        for id in [a, b] {
            assert_eq!(state_of(&conn, uid, id), "dropped");
        }
        assert_eq!(state_of(&conn, uid, other), "done", "a sibling of the dropped node is untouched");
    }

    #[test]
    fn now_holds_one_node_of_a_line_and_the_newest_takes_the_place() {
        let (conn, uid) = db_with_user();
        let course = task(&conn, uid, "course", None);
        let unit = task(&conn, uid, "unit", Some(course));
        let lesson = task(&conn, uid, "lesson", Some(unit));
        let sibling = task(&conn, uid, "sibling", Some(course));
        let flag = |id| get(&conn, uid, id).unwrap().unwrap().is_now;
        let now = |id| update(&conn, uid, id, &TaskPatch { is_now: Some(true), ..Default::default() }).unwrap().unwrap();

        now(course);
        let up = now(lesson);
        assert_eq!(up.adjusted.iter().map(|a| (a.id, a.field, a.by)).collect::<Vec<_>>(), [(course, "is_now", Some(lesson))]);
        assert!(flag(lesson) && !flag(course));
        now(sibling);
        assert!(flag(sibling) && flag(lesson), "cousins sit in Now together");
        let up = now(unit);
        assert_eq!(up.adjusted.iter().map(|a| a.id).collect::<Vec<_>>(), [lesson]);

        let x = task(&conn, uid, "x", None);
        now(x);
        let y = task(&conn, uid, "y", None);
        let full = update(&conn, uid, y, &TaskPatch { is_now: Some(true), ..Default::default() });
        assert!(matches!(full, Err(UpdateError::NowFull(_))), "{full:?}");
        let swap = now(course);
        let mut gone: Vec<i64> = swap.adjusted.iter().map(|a| a.id).collect();
        gone.sort_unstable();
        assert_eq!(gone, [unit, sibling], "a relative's place is free for the one that replaces it");
        assert_eq!(now_members(&conn, uid).unwrap(), [course, x]);
    }

    #[test]
    fn a_move_refuses_cycles_and_depth_and_flattens_what_would_sit_too_deep() {
        let (conn, uid) = db_with_user();
        let chain: Vec<i64> = (0..4).fold(Vec::new(), |mut acc, i| {
            let parent = acc.last().copied();
            acc.push(task(&conn, uid, &format!("level {i}"), parent));
            acc
        });
        let deepest = task(&conn, uid, "level 4", Some(chain[3]));
        let cycle = update(&conn, uid, chain[0], &TaskPatch { parent_id: Some(Some(chain[2])), ..Default::default() });
        assert!(matches!(cycle, Err(UpdateError::InvalidHierarchy(_))), "{cycle:?}");
        let loose = task(&conn, uid, "loose", None);
        let deep = update(&conn, uid, loose, &TaskPatch { parent_id: Some(Some(deepest)), ..Default::default() });
        assert!(matches!(deep, Err(UpdateError::InvalidHierarchy(_))), "{deep:?}");

        let other = task(&conn, uid, "other", None);
        let mid = task(&conn, uid, "mid", Some(other));
        let a = sized(&conn, uid, "a", Some(mid), Some(10));
        let b = sized(&conn, uid, "b", Some(a), Some(15));
        let moved = update(&conn, uid, other, &TaskPatch { parent_id: Some(Some(chain[2])), ..Default::default() })
            .unwrap()
            .unwrap();
        assert_eq!(moved.task.depth, 3);
        let flattened: Vec<i64> = moved.adjusted.iter().filter(|x| x.field == "depth").map(|x| x.id).collect();
        assert_eq!(flattened, [a, b]);
        let parent_of = |id| get(&conn, uid, id).unwrap().unwrap().parent_id;
        assert_eq!((parent_of(a), parent_of(b)), (Some(other), Some(other)));
        let siblings: Vec<i64> = node(&conn, uid, other).unwrap().unwrap().children.iter().map(|c| c.task.id).collect();
        assert_eq!(siblings, [mid, a, b]);
        assert_eq!(get(&conn, uid, other).unwrap().unwrap().duration_min, Some(15), "flattened parents give up the sizes they summed");
        assert_eq!(get(&conn, uid, b).unwrap().unwrap().root_id, chain[0]);
    }

    #[test]
    fn a_promoted_node_keeps_what_it_read_and_a_demoted_root_gives_it_up() {
        let (conn, uid) = db_with_user();
        let goal = crate::goals::create(&conn, uid, crate::goals::NewGoal { title: "apply".into(), ..Default::default() }).unwrap();
        let root = create(
            &conn,
            uid,
            NewTask { title: "essay".into(), category: Some("English".into()), goal_id: Some(goal.id), ..Default::default() },
            "manual",
            Actor::User,
        )
        .unwrap()
        .id;
        let unit = task(&conn, uid, "unit", Some(root));
        let part = task(&conn, uid, "part", Some(unit));
        let up = update(&conn, uid, unit, &TaskPatch { parent_id: Some(None), ..Default::default() }).unwrap().unwrap();
        assert_eq!((up.task.category.as_str(), up.task.goal_id, up.task.depth), ("English", Some(goal.id), 0));
        let p = get(&conn, uid, part).unwrap().unwrap();
        assert_eq!((p.root_id, p.depth, p.category.as_str()), (unit, 1, "English"));

        update(&conn, uid, unit, &TaskPatch { is_now: Some(true), ..Default::default() }).unwrap();
        let down = update(&conn, uid, unit, &TaskPatch { parent_id: Some(Some(root)), ..Default::default() }).unwrap().unwrap();
        let mut lost: Vec<&str> = down.adjusted.iter().map(|a| a.field).collect();
        lost.sort_unstable();
        assert_eq!(lost, ["category", "goal_id", "is_now"]);
        assert!(!down.task.is_now);
        assert_eq!(get(&conn, uid, part).unwrap().unwrap().depth, 2);
    }

    #[test]
    fn delete_snapshot_and_restore_cover_the_whole_subtree() {
        let (conn, uid) = db_with_user();
        let course = task(&conn, uid, "course", None);
        let unit = task(&conn, uid, "unit", Some(course));
        let lesson = sized(&conn, uid, "lesson", Some(unit), Some(30));
        let snap = snapshot(&conn, uid, course).unwrap();
        task(&conn, uid, "added later", Some(lesson));
        put_state(&conn, uid, unit, "dropped");
        restore(&conn, &snap).unwrap();
        let tree = node(&conn, uid, course).unwrap().unwrap();
        assert_eq!(tree.walk().iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["course", "unit", "lesson"]);
        assert_eq!(state_of(&conn, uid, lesson), "open");

        assert!(delete(&conn, uid, unit).unwrap());
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM tasks"), 1);
        assert_eq!(get(&conn, uid, course).unwrap().unwrap().duration_source, "user", "an emptied parent owns its last size");
    }

    #[test]
    fn the_queue_ranks_a_root_by_its_most_pressing_part_and_names_the_leaf() {
        let (conn, uid) = db_with_user();
        let now: jiff::Timestamp = "2026-11-05T12:00:00Z".parse().unwrap();
        let other = task(&conn, uid, "other", None);
        conn.execute("UPDATE tasks SET due_at = '2026-11-20T00:00:00Z' WHERE id = ?1", [other]).unwrap();
        let course = task(&conn, uid, "course", None);
        conn.execute("UPDATE tasks SET due_at = '2026-12-18T00:00:00Z' WHERE id = ?1", [course]).unwrap();
        let unit = task(&conn, uid, "unit", Some(course));
        conn.execute("UPDATE tasks SET due_at = '2026-11-06T00:00:00Z' WHERE id = ?1", [unit]).unwrap();
        let first = sized(&conn, uid, "first", Some(unit), Some(20));
        let second = sized(&conn, uid, "second", Some(unit), Some(35));
        put_state(&conn, uid, first, "done");

        let q = queue(&conn, uid, &jiff::tz::TimeZone::UTC, now, 10).unwrap();
        assert_eq!(q.iter().map(|e| e.task.task.id).collect::<Vec<_>>(), [course, other]);
        assert_eq!(q[0].reason, "due_soon");
        assert_eq!(q[0].leaf.as_ref().map(|l| l.id), Some(second));
        assert_eq!(q[0].step.as_ref().map(|l| l.id), Some(second));
        assert_eq!(q[0].path, ["course", "unit", "second"]);
        assert_eq!(q[0].planned_min, Some(35));
        assert!(q[1].leaf.is_none());
        assert_eq!(q[1].path, ["other"]);

        update(&conn, uid, second, &TaskPatch { is_now: Some(true), ..Default::default() }).unwrap();
        let q = queue(&conn, uid, &jiff::tz::TimeZone::UTC, now, 10).unwrap();
        assert_eq!(q[0].reason, "now", "a part in Now puts its tree first");
    }

    #[test]
    fn siblings_keep_the_order_they_are_given() {
        let (conn, uid) = db_with_user();
        let root = task(&conn, uid, "root", None);
        let a = task(&conn, uid, "a", Some(root));
        let b = task(&conn, uid, "b", Some(root));
        let c = task(&conn, uid, "c", Some(root));
        update(&conn, uid, c, &TaskPatch { position: Some(0), ..Default::default() }).unwrap();
        let order = |id| node(&conn, uid, id).unwrap().unwrap().children.iter().map(|n| n.task.id).collect::<Vec<_>>();
        assert_eq!(order(root), [c, a, b]);
        update(&conn, uid, c, &TaskPatch { position: Some(9), ..Default::default() }).unwrap();
        assert_eq!(order(root), [a, b, c]);
        assert_eq!(list(&conn, uid).unwrap()[0].children.iter().map(|n| n.task.id).collect::<Vec<_>>(), [a, b, c]);
    }
}
