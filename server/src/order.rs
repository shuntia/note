//! The day's task list: each date's tasks in the order Now offers them, a
//! timed one pointing at the block event that delivers it.

use crate::tasks::{self, QueueEntry};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

pub const MAX_ITEMS: usize = 60;
/// How soon a timed item's start may be for Now to offer it already.
pub const LEAD_MIN: i64 = 10;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Item {
    pub task_id: i64,
    pub title: String,
    /// The task a part belongs to.
    pub parent: Option<String>,
    /// The block delivering it, with its start and end.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
}

/// One stored row of a day, whatever its task's state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slot {
    pub task_id: i64,
    pub event_id: Option<i64>,
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
/// deleted, or whose block was dropped, has left it.
pub fn list(conn: &Connection, user_id: i64, date: jiff::civil::Date) -> rusqlite::Result<Vec<Item>> {
    let mut stmt = conn.prepare_cached(
        "SELECT t.id, t.title, p.title, e.id, e.wall_time, e.end_wall_time FROM day_items d
         JOIN tasks t ON t.id = d.task_id
         LEFT JOIN tasks p ON p.id = t.parent_id
         JOIN tasks r ON r.id = t.root_id
         LEFT JOIN events e ON e.id = d.event_id
         WHERE d.user_id = ?1 AND d.date = ?2 AND t.user_id = ?1
           AND t.state IN ('open','in_progress')
           AND (p.id IS NULL OR p.state IN ('open','in_progress'))
           AND r.state IN ('open','in_progress')
           AND (e.id IS NULL OR e.status != 'dropped')
         ORDER BY d.position",
    )?;
    let rows = stmt.query_map((user_id, date.to_string()), |r| {
        Ok(Item {
            task_id: r.get(0)?,
            title: r.get(1)?,
            parent: r.get(2)?,
            event_id: r.get(3)?,
            at: r.get(4)?,
            end: r.get(5)?,
        })
    })?;
    rows.collect()
}

pub fn ids(conn: &Connection, user_id: i64, date: jiff::civil::Date) -> rusqlite::Result<Vec<i64>> {
    Ok(list(conn, user_id, date)?.into_iter().map(|i| i.task_id).collect())
}

/// Every row of the day by position.
pub fn slots(conn: &Connection, user_id: i64, date: jiff::civil::Date) -> rusqlite::Result<Vec<Slot>> {
    let mut stmt = conn.prepare_cached(
        "SELECT task_id, event_id FROM day_items WHERE user_id = ?1 AND date = ?2 ORDER BY position",
    )?;
    let rows = stmt.query_map((user_id, date.to_string()), |r| Ok(Slot { task_id: r.get(0)?, event_id: r.get(1)? }))?;
    rows.collect()
}

/// The start of a slot's block while it is still live.
fn live_start(conn: &Connection, event_id: Option<i64>) -> rusqlite::Result<Option<i64>> {
    let Some(id) = event_id else { return Ok(None) };
    let wall: Option<String> = conn
        .query_row("SELECT wall_time FROM events WHERE id = ?1 AND status != 'dropped'", [id], |r| r.get(0))
        .optional()?;
    Ok(wall.map(|w| crate::templates::wall_minutes(&w)))
}

/// Stores `slots` as the whole day, in order: a slot whose block was dropped
/// leaves, and the timed ones are put in time order among the places timed
/// ones hold.
pub fn write(conn: &Connection, user_id: i64, date: jiff::civil::Date, slots: &[Slot]) -> rusqlite::Result<()> {
    let mut kept: Vec<(Slot, Option<i64>)> = Vec::with_capacity(slots.len());
    for s in slots {
        if kept.iter().any(|(k, _)| k.task_id == s.task_id) {
            continue;
        }
        let start = live_start(conn, s.event_id)?;
        if s.event_id.is_some() && start.is_none() {
            continue;
        }
        kept.push((*s, start));
    }
    let mut timed: Vec<(Slot, Option<i64>)> = kept.iter().copied().filter(|(_, t)| t.is_some()).collect();
    timed.sort_by_key(|(_, t)| *t);
    let mut timed = timed.into_iter();
    let ordered: Vec<Slot> = kept
        .iter()
        .map(|(s, t)| if t.is_some() { timed.next().map_or(*s, |(n, _)| n) } else { *s })
        .collect();
    conn.execute("DELETE FROM day_items WHERE user_id = ?1 AND date = ?2", (user_id, date.to_string()))?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO day_items (user_id, date, position, task_id, event_id) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (position, s) in ordered.iter().enumerate() {
        insert.execute((user_id, date.to_string(), position as i64, s.task_id, s.event_id))?;
    }
    Ok(())
}

/// Gives `task_id` the block `event_id` on that day: its row when it has one,
/// else a new one in time order.
pub fn attach(conn: &Connection, user_id: i64, date: jiff::civil::Date, task_id: i64, event_id: i64) -> rusqlite::Result<()> {
    let mut all = slots(conn, user_id, date)?;
    match all.iter_mut().find(|s| s.task_id == task_id) {
        Some(s) => s.event_id = Some(event_id),
        None => all.push(Slot { task_id, event_id: Some(event_id) }),
    }
    write(conn, user_id, date, &all)
}

/// Takes the row holding the block `event_id` off its day.
pub fn detach_event(conn: &Connection, event_id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM day_items WHERE event_id = ?1", [event_id]).map(|_| ())
}

fn checked(conn: &Connection, user_id: i64, task_ids: &[i64]) -> Result<(), OrderError> {
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
    Ok(())
}

/// Replaces the day's whole list of open items; every id must be one of the
/// user's open tasks, named once. A named item keeps its block, an open one
/// left out leaves the day with its block, and closed ones stay behind.
pub fn set(conn: &Connection, user_id: i64, date: jiff::civil::Date, task_ids: &[i64]) -> Result<Vec<Item>, OrderError> {
    let tx = conn.unchecked_transaction()?;
    let items = replace(&tx, user_id, date, task_ids)?;
    tx.commit()?;
    Ok(items)
}

/// `set` for a caller already inside a transaction.
pub fn replace(conn: &Connection, user_id: i64, date: jiff::civil::Date, task_ids: &[i64]) -> Result<Vec<Item>, OrderError> {
    checked(conn, user_id, task_ids)?;
    let before = slots(conn, user_id, date)?;
    let open: Vec<i64> = ids(conn, user_id, date)?;
    let mut after: Vec<Slot> = task_ids
        .iter()
        .map(|&id| before.iter().find(|s| s.task_id == id).copied().unwrap_or(Slot { task_id: id, event_id: None }))
        .collect();
    for s in &before {
        if task_ids.contains(&s.task_id) {
            continue;
        }
        if open.contains(&s.task_id) {
            drop_block(conn, s.event_id)?;
        } else {
            after.push(*s);
        }
    }
    write(conn, user_id, date, &after)?;
    Ok(list(conn, user_id, date)?)
}

/// Drops a block still waiting, so its task is no longer due then.
pub fn drop_block(conn: &Connection, event_id: Option<i64>) -> rusqlite::Result<()> {
    if let Some(id) = event_id {
        conn.execute(
            "UPDATE events SET status = 'dropped', decided_at = ?2
             WHERE id = ?1 AND status IN ('pending','snoozed','fired')",
            (id, jiff::Timestamp::now().to_string()),
        )?;
    }
    Ok(())
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

/// What Now offers next: today's open items whose block, if any, starts
/// within `LEAD_MIN`, one entry per tree, then the queue behind them.
pub fn candidates(
    conn: &Connection,
    user_id: i64,
    tz: &jiff::tz::TimeZone,
    now: jiff::Timestamp,
    limit: usize,
) -> rusqlite::Result<Vec<QueueEntry>> {
    let local = now.to_zoned(tz.clone());
    let today = local.date();
    let soon = i64::from(local.hour()) * 60 + i64::from(local.minute()) + LEAD_MIN;
    let mut out: Vec<QueueEntry> = Vec::new();
    for item in list(conn, user_id, today)? {
        if out.len() == limit {
            break;
        }
        if item.at.as_deref().is_some_and(|at| crate::templates::wall_minutes(at) > soon) {
            continue;
        }
        let Some(held) = tasks::get(conn, user_id, item.task_id)? else {
            continue;
        };
        if out.iter().any(|e| e.task.task.id == held.root_id) {
            continue;
        }
        let Some((task, leaf)) = tasks::start_point(conn, user_id, &held, now)? else {
            continue;
        };
        out.push(QueueEntry::new(task, leaf, "order"));
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
            .map(|e| (e.task.task.id, e.leaf.as_ref().map(|s| s.id), e.reason))
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
