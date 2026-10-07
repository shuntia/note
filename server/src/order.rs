use crate::tasks::{self, QueueEntry};
use rusqlite::Connection;
use serde::Serialize;

pub const MAX_ITEMS: usize = 30;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Item {
    pub task_id: i64,
    pub title: String,
    /// The task a step belongs to.
    pub parent: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum OrderError {
    #[error("no open task {0} for this user")]
    NotFound(i64),
    #[error("task {0} is named twice")]
    Twice(i64),
    #[error("an order holds at most {MAX_ITEMS} items")]
    TooMany,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

/// The day's open items by position; an item that is finished, dropped or
/// deleted, or a step whose task is, has left it.
pub fn list(
    conn: &Connection,
    user_id: i64,
    date: jiff::civil::Date,
) -> rusqlite::Result<Vec<Item>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.title, p.title FROM run_order o
         JOIN tasks t ON t.id = o.task_id
         LEFT JOIN tasks p ON p.id = t.parent_id
         WHERE o.user_id = ?1 AND o.date = ?2 AND t.user_id = ?1
           AND t.state IN ('open','in_progress')
           AND (p.id IS NULL OR p.state IN ('open','in_progress'))
         ORDER BY o.position",
    )?;
    let rows = stmt.query_map((user_id, date.to_string()), |r| {
        Ok(Item {
            task_id: r.get(0)?,
            title: r.get(1)?,
            parent: r.get(2)?,
        })
    })?;
    rows.collect()
}

pub fn ids(conn: &Connection, user_id: i64, date: jiff::civil::Date) -> rusqlite::Result<Vec<i64>> {
    Ok(list(conn, user_id, date)?
        .into_iter()
        .map(|i| i.task_id)
        .collect())
}

/// Replaces the day's whole order; every id must be one of the user's open
/// tasks or steps, named once.
pub fn set(
    conn: &Connection,
    user_id: i64,
    date: jiff::civil::Date,
    task_ids: &[i64],
) -> Result<Vec<Item>, OrderError> {
    if task_ids.len() > MAX_ITEMS {
        return Err(OrderError::TooMany);
    }
    let mut seen = std::collections::HashSet::new();
    for &id in task_ids {
        if !seen.insert(id) {
            return Err(OrderError::Twice(id));
        }
        let open = |t: &tasks::Task| matches!(t.state.as_str(), "open" | "in_progress");
        let Some(held) = tasks::get(conn, user_id, id)?.filter(open) else {
            return Err(OrderError::NotFound(id));
        };
        if let Some(parent) = held.parent_id {
            if !tasks::get(conn, user_id, parent)?.is_some_and(|p| open(&p)) {
                return Err(OrderError::NotFound(id));
            }
        }
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM run_order WHERE user_id = ?1 AND date = ?2",
        (user_id, date.to_string()),
    )?;
    for (position, id) in task_ids.iter().enumerate() {
        tx.execute(
            "INSERT INTO run_order (user_id, date, position, task_id) VALUES (?1, ?2, ?3, ?4)",
            (user_id, date.to_string(), position as i64, id),
        )?;
    }
    tx.commit()?;
    Ok(list(conn, user_id, date)?)
}

/// `ids` with `task_id` taken out and put back in front of `before`, or last;
/// `None` when `before` is not another item of `ids`.
pub fn placed(ids: &[i64], task_id: i64, before: Option<i64>) -> Option<Vec<i64>> {
    let mut out: Vec<i64> = ids.iter().copied().filter(|&i| i != task_id).collect();
    let at = match before {
        None => out.len(),
        Some(b) => out.iter().position(|&i| i == b)?,
    };
    out.insert(at, task_id);
    Some(out)
}

/// What Now offers next: today's order, one entry per task, then the queue
/// behind it.
pub fn candidates(
    conn: &Connection,
    user_id: i64,
    tz: &jiff::tz::TimeZone,
    now: jiff::Timestamp,
    limit: usize,
) -> rusqlite::Result<Vec<QueueEntry>> {
    let today = now.to_zoned(tz.clone()).date();
    let mut out: Vec<QueueEntry> = Vec::new();
    for item in list(conn, user_id, today)? {
        if out.len() == limit {
            break;
        }
        let Some(held) = tasks::get(conn, user_id, item.task_id)? else {
            continue;
        };
        let top_id = held.parent_id.unwrap_or(held.id);
        if out.iter().any(|e| e.task.task.id == top_id) {
            continue;
        }
        let Some((task, step)) = tasks::start_point(conn, user_id, held, now)? else {
            continue;
        };
        out.push(QueueEntry {
            planned_min: tasks::planned_minutes(&task.task, step.as_ref()),
            task,
            step,
            reason: "order",
            event_id: None,
        });
    }
    for entry in tasks::queue(conn, user_id, tz, now, limit)? {
        if out.len() == limit {
            break;
        }
        if out.iter().all(|e| e.task.task.id != entry.task.task.id) {
            out.push(entry);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{Actor, NewTask, QueueEntry};

    fn env() -> (Connection, i64) {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
        (conn, uid)
    }

    fn task(conn: &Connection, uid: i64, title: &str, parent: Option<i64>) -> i64 {
        crate::tasks::create(
            conn,
            uid,
            NewTask {
                title: title.into(),
                parent_id: parent,
                ..NewTask::default()
            },
            "manual",
            Actor::User,
        )
        .unwrap()
        .id
    }

    fn day() -> jiff::civil::Date {
        "2026-10-07".parse().unwrap()
    }

    fn close(conn: &Connection, id: i64, state: &str) {
        conn.execute("UPDATE tasks SET state = ?1 WHERE id = ?2", (state, id))
            .unwrap();
    }

    #[test]
    fn an_order_is_set_whole_and_read_back_by_position() {
        let (conn, uid) = env();
        let essay = task(&conn, uid, "essay", None);
        let laundry = task(&conn, uid, "laundry", None);
        let outline = task(&conn, uid, "outline", Some(essay));
        let items = set(&conn, uid, day(), &[laundry, outline, essay]).unwrap();
        assert_eq!(
            items.iter().map(|i| i.task_id).collect::<Vec<_>>(),
            [laundry, outline, essay]
        );
        assert_eq!(items[1].parent.as_deref(), Some("essay"));
        assert_eq!(items[0].parent, None);
        set(&conn, uid, day(), &[essay]).unwrap();
        assert_eq!(
            ids(&conn, uid, day()).unwrap(),
            [essay],
            "a set replaces the whole list"
        );
        assert!(
            ids(&conn, uid, day().tomorrow().unwrap())
                .unwrap()
                .is_empty(),
            "each day has its own"
        );
    }

    #[test]
    fn a_finished_or_deleted_item_leaves_the_order() {
        let (conn, uid) = env();
        let a = task(&conn, uid, "a", None);
        let b = task(&conn, uid, "b", None);
        let c = task(&conn, uid, "c", None);
        set(&conn, uid, day(), &[a, b, c]).unwrap();
        close(&conn, a, "done");
        close(&conn, b, "dropped");
        assert_eq!(ids(&conn, uid, day()).unwrap(), [c]);
        crate::tasks::delete(&conn, uid, c).unwrap();
        assert!(ids(&conn, uid, day()).unwrap().is_empty());
    }

    #[test]
    fn a_step_of_a_finished_task_leaves_with_it() {
        let (conn, uid) = env();
        let essay = task(&conn, uid, "essay", None);
        let outline = task(&conn, uid, "outline", Some(essay));
        set(&conn, uid, day(), &[outline]).unwrap();
        close(&conn, essay, "done");
        assert!(ids(&conn, uid, day()).unwrap().is_empty());
    }

    #[test]
    fn a_set_naming_a_foreign_closed_or_repeated_task_changes_nothing() {
        let (conn, uid) = env();
        let other = crate::auth::create_user(&conn, "bo", "p", false).unwrap();
        let a = task(&conn, uid, "a", None);
        let done = task(&conn, uid, "done", None);
        close(&conn, done, "done");
        let theirs = task(&conn, other, "theirs", None);
        set(&conn, uid, day(), &[a]).unwrap();
        assert!(
            matches!(set(&conn, uid, day(), &[a, theirs]), Err(OrderError::NotFound(id)) if id == theirs)
        );
        assert!(matches!(
            set(&conn, uid, day(), &[done]),
            Err(OrderError::NotFound(_))
        ));
        assert!(matches!(
            set(&conn, uid, day(), &[404]),
            Err(OrderError::NotFound(404))
        ));
        assert!(matches!(
            set(&conn, uid, day(), &[a, a]),
            Err(OrderError::Twice(_))
        ));
        let many: Vec<i64> = (0..=MAX_ITEMS)
            .map(|i| task(&conn, uid, &format!("t{i}"), None))
            .collect();
        assert!(matches!(
            set(&conn, uid, day(), &many),
            Err(OrderError::TooMany)
        ));
        assert_eq!(ids(&conn, uid, day()).unwrap(), [a]);
    }

    #[test]
    fn a_set_naming_an_open_step_of_a_closed_task_changes_nothing() {
        let (conn, uid) = env();
        let essay = task(&conn, uid, "essay", None);
        let outline = task(&conn, uid, "outline", Some(essay));
        close(&conn, essay, "dropped");
        assert!(matches!(set(&conn, uid, day(), &[outline]), Err(OrderError::NotFound(id)) if id == outline));
        assert!(ids(&conn, uid, day()).unwrap().is_empty());
    }

    #[test]
    fn two_steps_of_one_task_offer_it_once_from_the_first() {
        let (conn, uid) = env();
        let now: jiff::Timestamp = "2026-10-07T12:00:00Z".parse().unwrap();
        let essay = task(&conn, uid, "essay", None);
        let outline = task(&conn, uid, "outline", Some(essay));
        let draft = task(&conn, uid, "draft", Some(essay));
        set(&conn, uid, day(), &[draft, outline]).unwrap();
        let got = candidates(&conn, uid, &jiff::tz::TimeZone::UTC, now, 5).unwrap();
        assert_eq!(picked(&got), [(essay, Some(draft), "order")]);
    }

    #[test]
    fn placing_puts_an_item_in_front_of_another_or_last() {
        assert_eq!(placed(&[1, 2, 3], 3, Some(1)), Some(vec![3, 1, 2]));
        assert_eq!(placed(&[1, 2, 3], 1, None), Some(vec![2, 3, 1]));
        assert_eq!(
            placed(&[1, 2], 9, Some(2)),
            Some(vec![1, 9, 2]),
            "a new item joins"
        );
        assert_eq!(
            placed(&[1, 2], 1, Some(1)),
            None,
            "nothing goes in front of itself"
        );
        assert_eq!(
            placed(&[1, 2], 9, Some(7)),
            None,
            "before must be in the order"
        );
    }

    fn picked(c: &[QueueEntry]) -> Vec<(i64, Option<i64>, &'static str)> {
        c.iter()
            .map(|e| (e.task.task.id, e.step.as_ref().map(|s| s.id), e.reason))
            .collect()
    }

    #[test]
    fn now_takes_the_head_of_the_order_then_the_queue() {
        let (conn, uid) = env();
        let now: jiff::Timestamp = "2026-10-07T12:00:00Z".parse().unwrap();
        let utc = jiff::tz::TimeZone::UTC;
        let oldest = task(&conn, uid, "oldest", None);
        let essay = task(&conn, uid, "essay", None);
        let outline = task(&conn, uid, "outline", Some(essay));
        let laundry = task(&conn, uid, "laundry", None);

        let first = candidates(&conn, uid, &utc, now, 5).unwrap();
        assert_eq!(first[0].reason, "oldest", "no order: the queue");

        set(&conn, uid, day(), &[outline, laundry]).unwrap();
        let got = candidates(&conn, uid, &utc, now, 5).unwrap();
        assert_eq!(
            picked(&got)[..2],
            [(essay, Some(outline), "order"), (laundry, None, "order")]
        );
        assert_eq!(
            got[2].task.task.id, oldest,
            "the queue fills in behind the order"
        );
        assert_eq!(got.len(), 3, "a task is offered once");
        assert_eq!(candidates(&conn, uid, &utc, now, 1).unwrap().len(), 1);

        close(&conn, outline, "done");
        close(&conn, laundry, "done");
        assert!(
            candidates(&conn, uid, &utc, now, 5)
                .unwrap()
                .iter()
                .all(|e| e.reason != "order"),
            "an exhausted order falls back to the queue"
        );
    }
}
