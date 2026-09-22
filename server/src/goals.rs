use crate::tasks::UpdateError;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// A goal is open until it is reached or let go of.
pub const STATES: &[&str] = &["open", "done", "dropped"];

const MAX_TITLE_BYTES: usize = 200;
const MAX_DESCRIPTION_BYTES: usize = 8 * 1024;

/// One goal with how far its tasks have got and which of them falls due next,
/// so a row can be drawn without a second round trip.
#[derive(Debug, Serialize)]
pub struct Goal {
    pub id: i64,
    pub title: String,
    pub description: String,
    /// RFC 3339 UTC, like a task's.
    pub due_at: Option<String>,
    pub state: String,
    pub created_at: String,
    pub updated_at: String,
    /// Tasks hanging from this goal that are not dropped, and how many of those
    /// are done.
    pub tasks: i64,
    pub done_tasks: i64,
    pub next_task_id: Option<i64>,
    pub next_task_title: Option<String>,
    pub next_due_at: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewGoal {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "crate::tasks::present")]
    pub due_at: Option<Option<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalPatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub state: Option<String>,
    #[serde(default, deserialize_with = "crate::tasks::present")]
    pub due_at: Option<Option<String>>,
}

fn now() -> String {
    jiff::Timestamp::now().to_string()
}

fn checked_title(raw: &str) -> Result<String, UpdateError> {
    let title = raw.trim();
    if title.is_empty() || title.len() > MAX_TITLE_BYTES {
        return Err(UpdateError::Invalid(format!("title must be 1..={MAX_TITLE_BYTES} bytes")));
    }
    Ok(title.to_owned())
}

fn checked_description(raw: &str) -> Result<(), UpdateError> {
    if raw.len() > MAX_DESCRIPTION_BYTES {
        return Err(UpdateError::Invalid(format!(
            "description must be at most {MAX_DESCRIPTION_BYTES} bytes"
        )));
    }
    Ok(())
}

fn checked_due(raw: &str) -> Result<String, UpdateError> {
    raw.parse::<jiff::Timestamp>().map(|t| t.to_string()).map_err(|_| {
        UpdateError::Invalid(format!("due_at must be an RFC 3339 instant, got {raw:?}"))
    })
}

fn checked_state(raw: &str) -> Result<String, UpdateError> {
    if !STATES.contains(&raw) {
        return Err(UpdateError::Invalid(format!("state must be one of {}", STATES.join(", "))));
    }
    Ok(raw.to_owned())
}

const COLS: &str = "g.id, g.title, g.description, g.due_at, g.state, g.created_at, g.updated_at,
    (SELECT COUNT(*) FROM tasks t WHERE t.goal_id = g.id AND t.state != 'dropped'),
    (SELECT COUNT(*) FROM tasks t WHERE t.goal_id = g.id AND t.state = 'done'),
    n.id, n.title, n.due_at";

/// The goal's own row, joined to the unfinished task of its whose deadline
/// comes first.
const FROM: &str = "goals g LEFT JOIN tasks n ON n.id = (
    SELECT t.id FROM tasks t
    WHERE t.goal_id = g.id AND t.state IN ('open','in_progress')
    ORDER BY t.due_at IS NULL, t.due_at, t.id LIMIT 1)";

fn row_to_goal(r: &rusqlite::Row) -> rusqlite::Result<Goal> {
    Ok(Goal {
        id: r.get(0)?,
        title: r.get(1)?,
        description: r.get(2)?,
        due_at: r.get(3)?,
        state: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
        tasks: r.get(7)?,
        done_tasks: r.get(8)?,
        next_task_id: r.get(9)?,
        next_task_title: r.get(10)?,
        next_due_at: r.get(11)?,
    })
}

pub fn get(conn: &Connection, user_id: i64, goal_id: i64) -> rusqlite::Result<Option<Goal>> {
    conn.query_row(
        &format!("SELECT {COLS} FROM {FROM} WHERE g.id = ?1 AND g.user_id = ?2"),
        (goal_id, user_id),
        row_to_goal,
    )
    .optional()
}

/// The user's goals, the soonest deadline first and the undated ones last.
/// `state` of `None` lists the open ones; `Some("any")` lists them all.
pub fn list(conn: &Connection, user_id: i64, state: Option<&str>) -> Result<Vec<Goal>, UpdateError> {
    let filter = match state {
        None => "g.state = 'open'".to_string(),
        Some("any") => "1".to_string(),
        Some(s) => {
            checked_state(s)?;
            format!("g.state = '{s}'")
        }
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM {FROM} WHERE g.user_id = ?1 AND {filter}
         ORDER BY g.due_at IS NULL, g.due_at, g.id"
    ))?;
    let rows = stmt.query_map([user_id], row_to_goal)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn create(conn: &Connection, user_id: i64, new: NewGoal) -> Result<Goal, UpdateError> {
    let title = checked_title(&new.title)?;
    let description = new.description.unwrap_or_default();
    checked_description(&description)?;
    let due_at = new.due_at.flatten().as_deref().map(checked_due).transpose()?;
    conn.execute(
        "INSERT INTO goals (user_id, title, description, due_at, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        rusqlite::params![user_id, &title, &description, due_at, now()],
    )?;
    let id = conn.last_insert_rowid();
    Ok(get(conn, user_id, id)?.expect("row was just created"))
}

/// `Ok(None)` when the goal is not this user's.
pub fn update(
    conn: &Connection,
    user_id: i64,
    goal_id: i64,
    patch: GoalPatch,
) -> Result<Option<Goal>, UpdateError> {
    let title = patch.title.as_deref().map(checked_title).transpose()?;
    if let Some(d) = &patch.description {
        checked_description(d)?;
    }
    let state = patch.state.as_deref().map(checked_state).transpose()?;
    let due_at = match &patch.due_at {
        Some(Some(raw)) => Some(Some(checked_due(raw)?)),
        other => other.as_ref().map(|_| None),
    };
    if get(conn, user_id, goal_id)?.is_none() {
        return Ok(None);
    }
    conn.execute(
        "UPDATE goals SET
            title = COALESCE(?1, title),
            description = COALESCE(?2, description),
            state = COALESCE(?3, state),
            due_at = COALESCE(?4, CASE WHEN ?5 THEN NULL ELSE due_at END),
            updated_at = ?6
         WHERE id = ?7",
        rusqlite::params![
            &title,
            &patch.description,
            &state,
            due_at.clone().flatten(),
            due_at.is_some(),
            now(),
            goal_id,
        ],
    )?;
    get(conn, user_id, goal_id).map_err(UpdateError::from)
}

/// Removes the goal; the tasks that hung from it stay, without one.
pub fn delete(conn: &Connection, user_id: i64, goal_id: i64) -> rusqlite::Result<bool> {
    let tx = conn.unchecked_transaction()?;
    let mine: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM goals WHERE id = ?1 AND user_id = ?2)",
        (goal_id, user_id),
        |r| r.get(0),
    )?;
    if !mine {
        return Ok(false);
    }
    tx.execute("UPDATE tasks SET goal_id = NULL WHERE goal_id = ?1", [goal_id])?;
    tx.execute("DELETE FROM goals WHERE id = ?1", [goal_id])?;
    tx.commit()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_user() -> (Connection, i64) {
        let conn = crate::db::open_memory().unwrap();
        let id = crate::auth::create_user(&conn, "aki", "pw", false).unwrap();
        (conn, id)
    }

    fn task(conn: &Connection, uid: i64, title: &str, goal: Option<i64>, due: Option<&str>) -> i64 {
        crate::tasks::create(
            conn,
            uid,
            crate::tasks::NewTask {
                title: title.into(),
                goal_id: goal,
                due_at: due.map(|d| Some(d.to_string())),
                ..Default::default()
            },
            "manual",
            crate::tasks::Actor::User,
        )
        .unwrap()
        .id
    }

    #[test]
    fn a_goal_counts_its_tasks_and_names_the_one_due_next() {
        let (conn, uid) = db_with_user();
        let goal = create(
            &conn,
            uid,
            NewGoal { title: "  get in  ".into(), due_at: Some(Some("2026-11-01T12:00:00Z".into())), ..Default::default() },
        )
        .unwrap();
        assert_eq!(goal.title, "get in");
        assert_eq!((goal.tasks, goal.done_tasks), (0, 0));
        assert!(goal.next_task_id.is_none());

        task(&conn, uid, "essay", Some(goal.id), Some("2026-10-25T12:00:00Z"));
        let soon = task(&conn, uid, "form", Some(goal.id), Some("2026-10-02T12:00:00Z"));
        let done = task(&conn, uid, "fee", Some(goal.id), None);
        crate::tasks::update(
            &conn,
            uid,
            done,
            crate::tasks::TaskPatch { state: Some("done".into()), ..Default::default() },
        )
        .unwrap();
        task(&conn, uid, "unrelated", None, None);

        let goal = get(&conn, uid, goal.id).unwrap().unwrap();
        assert_eq!((goal.tasks, goal.done_tasks), (3, 1));
        assert_eq!(goal.next_task_id, Some(soon));
        assert_eq!(goal.next_task_title.as_deref(), Some("form"));
        assert_eq!(goal.next_due_at.as_deref(), Some("2026-10-02T12:00:00Z"));
    }

    #[test]
    fn listing_shows_the_open_goals_soonest_first() {
        let (conn, uid) = db_with_user();
        let make = |title: &str, due: Option<&str>| {
            create(
                &conn,
                uid,
                NewGoal {
                    title: title.into(),
                    due_at: due.map(|d| Some(d.to_string())),
                    ..Default::default()
                },
            )
            .unwrap()
            .id
        };
        let undated = make("someday", None);
        let late = make("november", Some("2026-11-01T12:00:00Z"));
        let early = make("october", Some("2026-10-01T12:00:00Z"));
        update(&conn, uid, late, GoalPatch { state: Some("done".into()), ..Default::default() })
            .unwrap();

        let ids = |state: Option<&str>| -> Vec<i64> {
            list(&conn, uid, state).unwrap().into_iter().map(|g| g.id).collect()
        };
        assert_eq!(ids(None), vec![early, undated]);
        assert_eq!(ids(Some("any")), vec![early, late, undated]);
        assert_eq!(ids(Some("done")), vec![late]);
        assert!(list(&conn, uid, Some("someday")).is_err());
    }

    #[test]
    fn a_patch_touches_only_what_it_names_and_null_clears_the_date() {
        let (conn, uid) = db_with_user();
        let goal = create(
            &conn,
            uid,
            NewGoal {
                title: "apply".into(),
                description: Some("the whole application".into()),
                due_at: Some(Some("2026-11-01T12:00:00Z".into())),
            },
        )
        .unwrap();

        let after =
            update(&conn, uid, goal.id, GoalPatch { title: Some("apply early".into()), ..Default::default() })
                .unwrap()
                .unwrap();
        assert_eq!(after.title, "apply early");
        assert_eq!(after.description, "the whole application");
        assert_eq!(after.due_at.as_deref(), Some("2026-11-01T12:00:00Z"));

        let after =
            update(&conn, uid, goal.id, GoalPatch { due_at: Some(None), ..Default::default() })
                .unwrap()
                .unwrap();
        assert!(after.due_at.is_none());
        assert!(update(&conn, uid, goal.id, GoalPatch { title: Some("  ".into()), ..Default::default() })
            .is_err());
    }

    #[test]
    fn deleting_a_goal_leaves_its_tasks_behind_without_one() {
        let (conn, uid) = db_with_user();
        let goal = create(&conn, uid, NewGoal { title: "apply".into(), ..Default::default() }).unwrap();
        let id = task(&conn, uid, "essay", Some(goal.id), None);
        assert!(delete(&conn, uid, goal.id).unwrap());
        assert!(!delete(&conn, uid, goal.id).unwrap());
        let task = crate::tasks::get(&conn, uid, id).unwrap().unwrap();
        assert!(task.goal_id.is_none() && task.goal_title.is_none());
    }

    #[test]
    fn another_users_goal_is_out_of_reach() {
        let (conn, uid) = db_with_user();
        let bo = crate::auth::create_user(&conn, "bo", "pw", false).unwrap();
        let theirs = create(&conn, bo, NewGoal { title: "theirs".into(), ..Default::default() }).unwrap();
        assert!(get(&conn, uid, theirs.id).unwrap().is_none());
        assert!(update(&conn, uid, theirs.id, GoalPatch { title: Some("mine".into()), ..Default::default() })
            .unwrap()
            .is_none());
        assert!(!delete(&conn, uid, theirs.id).unwrap());
        assert!(list(&conn, uid, Some("any")).unwrap().is_empty());
    }
}
