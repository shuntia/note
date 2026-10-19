//! The buttons a message carries, pressed from a push notification or a
//! Matrix reaction. Each delivery keeps its buttons under keys of its own;
//! pressing one spends every button of that delivery.

use crate::channels::Action;
use rusqlite::{Connection, OptionalExtension};

/// How long a delivered button can still be pressed.
pub const TTL_HOURS: i64 = 36;

/// A key a push notification carries: 128 random bits, hex.
pub fn push_key() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

pub fn is_push_key(key: &str) -> bool {
    key.len() == 32 && key.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Keeps one delivery's buttons, `keys[i]` naming `actions[i]`, and forgets
/// buttons past their time.
pub fn keep(
    conn: &Connection,
    user_id: i64,
    batch: &str,
    keys: &[String],
    actions: &[Action],
    now: jiff::Timestamp,
) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM message_actions WHERE expires_at <= ?1", [now.to_string()])?;
    let expires = (now + jiff::Span::new().hours(TTL_HOURS)).to_string();
    for (key, action) in keys.iter().zip(actions) {
        conn.execute(
            "INSERT OR REPLACE INTO message_actions (key, batch, user_id, data, expires_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            (key, batch, user_id, &action.data, &expires),
        )?;
    }
    Ok(())
}

/// The user and the button `key` names, spending its delivery's buttons; none
/// once spent or past its time.
pub fn take(conn: &Connection, key: &str, now: jiff::Timestamp) -> rusqlite::Result<Option<(i64, String)>> {
    let row: Option<(String, i64, String)> = conn
        .query_row(
            "SELECT batch, user_id, data FROM message_actions WHERE key = ?1 AND expires_at > ?2",
            (key, now.to_string()),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((batch, user_id, data)) = row else { return Ok(None) };
    conn.execute("DELETE FROM message_actions WHERE batch = ?1", [batch])?;
    Ok(Some((user_id, data)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Done {
    Done,
    Snoozed(i64),
    Dropped,
    Carried(usize),
    Started,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Refused {
    #[error("not an action")]
    Unknown,
    #[error("nothing of this user's to act on")]
    NotFound,
    #[error("already settled")]
    Settled,
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Internal(String),
}

fn internal(e: impl std::fmt::Display) -> Refused {
    Refused::Internal(e.to_string())
}

/// Carries out a pressed button's `data` for the user it was given to.
pub fn perform(
    conn: &Connection,
    config_dir: &std::path::Path,
    user_id: i64,
    username: &str,
    data: &str,
    now: jiff::Timestamp,
) -> Result<Done, Refused> {
    let parts: Vec<&str> = data.split(':').collect();
    let id = |s: &str| s.parse::<i64>().map_err(|_| Refused::Unknown);
    match parts.as_slice() {
        ["ev", "done", ev] => settle(conn, user_id, id(ev)?, "done").map(|()| Done::Done),
        ["ev", "drop", ev] => settle(conn, user_id, id(ev)?, "dropped").map(|()| Done::Dropped),
        ["ev", "snooze", ev, min] => {
            let min = id(min)?;
            snooze_from_now(conn, config_dir, username, user_id, id(ev)?, min, now).map(|()| Done::Snoozed(min))
        }
        ["carry", date] => {
            let date: jiff::civil::Date = date.parse().map_err(|_| Refused::Unknown)?;
            let today = now.to_zoned(crate::triggers::timezone(config_dir, username)).date();
            if date < today {
                return Err(Refused::Settled);
            }
            crate::plan::carry(conn, config_dir, username, user_id, date, now).map(Done::Carried).map_err(internal)
        }
        ["block", "start", ev] => start_block(conn, config_dir, user_id, username, id(ev)?, now).map(|()| Done::Started),
        _ => Err(Refused::Unknown),
    }
}

fn settle(conn: &Connection, user_id: i64, event_id: i64, status: &str) -> Result<(), Refused> {
    match crate::plan::set_status(conn, user_id, event_id, status) {
        Ok(Some(())) => Ok(()),
        Ok(None) => Err(Refused::NotFound),
        Err(e) => Err(internal(e)),
    }
}

/// Snoozes to `min` minutes from now rather than from the event's own time,
/// which has already gone by. A trigger that spoke has settled, so snoozing it
/// asks again.
fn snooze_from_now(
    conn: &Connection,
    config_dir: &std::path::Path,
    username: &str,
    user_id: i64,
    event_id: i64,
    min: i64,
    now: jiff::Timestamp,
) -> Result<(), Refused> {
    let row: Option<(String, String, String, String)> = conn
        .query_row(
            "SELECT e.wall_time, e.status, e.kind, p.date FROM events e JOIN plans p ON p.id = e.plan_id
             WHERE e.id = ?1 AND p.user_id = ?2",
            (event_id, user_id),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(internal)?;
    let Some((wall, status, kind, date)) = row else { return Err(Refused::NotFound) };
    let local = now.to_zoned(crate::triggers::timezone(config_dir, username));
    let wall_min = crate::plan::parse_minutes(&wall).map_err(internal)?;
    let from = if date == local.date().to_string() {
        wall_min.max(i64::from(local.hour()) * 60 + i64::from(local.minute()))
    } else {
        wall_min
    };
    if kind == crate::triggers::KIND && status == "done" {
        conn.execute("UPDATE events SET status = 'fired' WHERE id = ?1", [event_id]).map_err(internal)?;
    }
    match crate::plan::snooze(conn, user_id, event_id, from - wall_min + min) {
        Ok(Some(())) => Ok(()),
        Ok(None) => Err(Refused::NotFound),
        Err(crate::plan::ShiftError::Decided { .. }) => Err(Refused::Settled),
        Err(e) => Err(Refused::Invalid(e.to_string())),
    }
}

/// Starts a work session on the block's task, for the block's length.
fn start_block(
    conn: &Connection,
    config_dir: &std::path::Path,
    user_id: i64,
    username: &str,
    event_id: i64,
    now: jiff::Timestamp,
) -> Result<(), Refused> {
    let row: Option<(i64, String, String, Option<String>)> = conn
        .query_row(
            "SELECT t.id, t.title, e.wall_time, e.end_wall_time FROM events e
             JOIN plans p ON p.id = e.plan_id
             JOIN event_tasks et ON et.event_id = e.id
             JOIN tasks t ON t.id = et.task_id
             WHERE e.id = ?1 AND p.user_id = ?2",
            (event_id, user_id),
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(internal)?;
    let Some((task_id, title, start, end)) = row else { return Err(Refused::NotFound) };
    if crate::triggers::open_work_session(conn, user_id).map_err(internal)?.is_some() {
        return Err(Refused::Settled);
    }
    let planned = end
        .and_then(|end| Some(crate::plan::parse_minutes(&end).ok()? - crate::plan::parse_minutes(&start).ok()?))
        .filter(|m| *m > 0);
    let new = crate::work::NewSession {
        task_id: Some(task_id),
        event_id: Some(event_id),
        title,
        planned_min: planned,
        step_index: None,
        step_count: None,
        step_name: None,
        notes: None,
    };
    match crate::work::start(conn, config_dir, user_id, username, new, now) {
        Ok(_) => Ok(()),
        Err(crate::work::StartError::Invalid(m)) => Err(Refused::Invalid(m)),
        Err(e) => Err(internal(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> (Connection, tempfile::TempDir, i64) {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aitest", "p", false).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("defaults/user.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n").unwrap();
        (conn, tmp, uid)
    }

    fn at(ts: &str) -> jiff::Timestamp {
        ts.parse().unwrap()
    }

    fn event(conn: &Connection, uid: i64, date: &str, kind: &str, wall: &str, status: &str) -> i64 {
        conn.execute(
            "INSERT OR IGNORE INTO plans (user_id, date, created_at) VALUES (?1, ?2, 'x')",
            (uid, date),
        )
        .unwrap();
        let plan: i64 =
            conn.query_row("SELECT id FROM plans WHERE user_id = ?1 AND date = ?2", (uid, date), |r| r.get(0)).unwrap();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, status) VALUES (?1, ?2, ?3, ?3, ?4)",
            (plan, kind, wall, status),
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn row(conn: &Connection, id: i64) -> (String, String) {
        conn.query_row("SELECT status, wall_time FROM events WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
    }

    fn actions(data: &[&str]) -> Vec<Action> {
        data.iter().map(|d| Action { label: "x".into(), data: (*d).into() }).collect()
    }

    #[test]
    fn one_press_spends_every_button_of_its_delivery() {
        let (conn, _tmp, uid) = env();
        let now = at("2026-10-18T09:00:00Z");
        let keys = vec![push_key(), push_key()];
        keep(&conn, uid, "b1", &keys, &actions(&["ev:done:1", "ev:drop:1"]), now).unwrap();
        keep(&conn, uid, "b2", &[push_key()], &actions(&["ev:done:2"]), now).unwrap();
        assert_eq!(take(&conn, &keys[1], now).unwrap(), Some((uid, "ev:drop:1".into())));
        assert_eq!(take(&conn, &keys[0], now).unwrap(), None);
        let left: i64 = conn.query_row("SELECT COUNT(*) FROM message_actions", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 1, "another delivery keeps its buttons");
    }

    #[test]
    fn a_button_past_its_time_presses_nothing() {
        let (conn, _tmp, uid) = env();
        let now = at("2026-10-18T09:00:00Z");
        let key = push_key();
        assert!(is_push_key(&key) && !is_push_key("mx:$ev:✅"));
        keep(&conn, uid, "b", std::slice::from_ref(&key), &actions(&["ev:done:1"]), now).unwrap();
        assert_eq!(take(&conn, &key, now + jiff::Span::new().hours(TTL_HOURS)).unwrap(), None);
    }

    #[test]
    fn done_and_drop_settle_only_the_users_own_event() {
        let (conn, tmp, uid) = env();
        let other = crate::auth::create_user(&conn, "shuntia", "p", false).unwrap();
        let now = at("2026-10-18T09:20:00Z");
        let mine = event(&conn, uid, "2026-10-18", "checkin", "09:00", "fired");
        let theirs = event(&conn, other, "2026-10-18", "checkin", "09:00", "fired");
        assert_eq!(perform(&conn, tmp.path(), uid, "aitest", &format!("ev:done:{mine}"), now), Ok(Done::Done));
        assert_eq!(row(&conn, mine).0, "done");
        assert_eq!(
            perform(&conn, tmp.path(), uid, "aitest", &format!("ev:drop:{theirs}"), now),
            Err(Refused::NotFound)
        );
        assert_eq!(row(&conn, theirs).0, "fired");
        assert_eq!(perform(&conn, tmp.path(), uid, "aitest", "rm:-rf", now), Err(Refused::Unknown));
    }

    #[test]
    fn snooze_counts_from_now_and_asks_a_spoken_trigger_again() {
        let (conn, tmp, uid) = env();
        let now = at("2026-10-18T09:40:00Z");
        let checkin = event(&conn, uid, "2026-10-18", "checkin", "09:00", "fired");
        assert_eq!(
            perform(&conn, tmp.path(), uid, "aitest", &format!("ev:snooze:{checkin}:15"), now),
            Ok(Done::Snoozed(15))
        );
        assert_eq!(row(&conn, checkin), ("snoozed".into(), "09:55".into()));

        let spoke = event(&conn, uid, "2026-10-18", "trigger", "09:30", "done");
        perform(&conn, tmp.path(), uid, "aitest", &format!("ev:snooze:{spoke}:15"), now).unwrap();
        assert_eq!(row(&conn, spoke), ("snoozed".into(), "09:55".into()));

        let settled = event(&conn, uid, "2026-10-18", "checkin", "09:00", "done");
        assert_eq!(
            perform(&conn, tmp.path(), uid, "aitest", &format!("ev:snooze:{settled}:15"), now),
            Err(Refused::Settled)
        );
    }

    #[test]
    fn carry_moves_a_day_still_open_and_refuses_one_that_is_over() {
        let (conn, tmp, uid) = env();
        let now = at("2026-10-18T21:30:00Z");
        crate::plan::ensure(&conn, tmp.path(), "aitest", uid, "2026-10-18".parse().unwrap()).unwrap();
        assert!(matches!(perform(&conn, tmp.path(), uid, "aitest", "carry:2026-10-18", now), Ok(Done::Carried(_))));
        assert_eq!(perform(&conn, tmp.path(), uid, "aitest", "carry:2026-10-17", now), Err(Refused::Settled));
    }
}
