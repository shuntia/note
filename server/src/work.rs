use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;

/// How long after a session starts its first progress check lands, when the
/// session named no length.
pub const DEFAULT_FIRST_CHECK_MIN: i64 = 25;
pub const MAX_TITLE_BYTES: usize = 200;
pub const MAX_PLANNED_MIN: i64 = 12 * 60;

#[derive(Debug, Serialize)]
pub struct Session {
    pub id: i64,
    pub task_id: Option<i64>,
    pub event_id: Option<i64>,
    pub title: String,
    pub planned_min: Option<i64>,
    pub started_at: String,
}

#[derive(Debug, Default)]
pub struct NewSession {
    pub task_id: Option<i64>,
    pub event_id: Option<i64>,
    pub title: String,
    pub planned_min: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// The session the user is in, if any.
pub fn open(conn: &Connection, user_id: i64) -> rusqlite::Result<Option<Session>> {
    conn.query_row(
        "SELECT id, task_id, event_id, title, planned_min, started_at FROM work_sessions
         WHERE user_id = ?1 AND ended_at IS NULL ORDER BY id DESC LIMIT 1",
        [user_id],
        |r| {
            Ok(Session {
                id: r.get(0)?,
                task_id: r.get(1)?,
                event_id: r.get(2)?,
                title: r.get(3)?,
                planned_min: r.get(4)?,
                started_at: r.get(5)?,
            })
        },
    )
    .optional()
}

/// Opens a session — stopping whatever was still running — and lays its first
/// progress check itself, so accountability does not wait on the agent's next
/// turn. A task or event named by someone else is refused.
pub fn start(
    conn: &Connection,
    config_dir: &Path,
    user_id: i64,
    username: &str,
    new: NewSession,
    now: jiff::Timestamp,
) -> Result<Session, StartError> {
    let title = new.title.trim();
    if title.is_empty() || title.len() > MAX_TITLE_BYTES {
        return Err(StartError::Invalid(format!(
            "title must be 1 to {MAX_TITLE_BYTES} bytes"
        )));
    }
    if let Some(min) = new.planned_min {
        if !(1..=MAX_PLANNED_MIN).contains(&min) {
            return Err(StartError::Invalid(format!(
                "planned_min must be 1 to {MAX_PLANNED_MIN}"
            )));
        }
    }
    if let Some(id) = new.task_id {
        if crate::tasks::get(conn, user_id, id)?.is_none() {
            return Err(StartError::Invalid(format!("no task {id}")));
        }
    }
    if let Some(id) = new.event_id {
        if crate::plan::event_gate(conn, user_id, id).map_err(StartError::Other)?.is_none() {
            return Err(StartError::Invalid(format!("no event {id}")));
        }
    }
    let tx = conn.unchecked_transaction()?;
    end(conn, user_id, None, "stopped", now).map_err(StartError::Other)?;
    conn.execute(
        "INSERT INTO work_sessions (user_id, task_id, event_id, title, planned_min, started_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        (user_id, new.task_id, new.event_id, title, new.planned_min, now.to_string()),
    )?;
    let id = conn.last_insert_rowid();
    lay_first_check(conn, config_dir, user_id, username, id, title, new.planned_min, now)
        .map_err(StartError::Other)?;
    crate::log::record(conn, Some(user_id), "work_session_started", &format!("session {id} {title:?}"))
        .map_err(StartError::Other)?;
    tx.commit()?;
    Ok(Session {
        id,
        task_id: new.task_id,
        event_id: new.event_id,
        title: title.to_string(),
        planned_min: new.planned_min,
        started_at: now.to_string(),
    })
}

/// Halfway through, or 25 minutes in, whichever comes first.
fn first_check_minutes(planned_min: Option<i64>) -> i64 {
    planned_min
        .map_or(DEFAULT_FIRST_CHECK_MIN, |min| (min / 2).min(DEFAULT_FIRST_CHECK_MIN))
        .max(1)
}

#[allow(clippy::too_many_arguments)]
fn lay_first_check(
    conn: &Connection,
    config_dir: &Path,
    user_id: i64,
    username: &str,
    session_id: i64,
    title: &str,
    planned_min: Option<i64>,
    now: jiff::Timestamp,
) -> Result<()> {
    let tz = crate::triggers::timezone(config_dir, username);
    let at = (now + jiff::Span::new().minutes(first_check_minutes(planned_min))).to_zoned(tz);
    let plan_id = crate::plan::ensure(conn, config_dir, username, user_id, at.date())?;
    let wall = format!("{:02}:{:02}", at.hour(), at.minute());
    let event_id = crate::triggers::insert(
        conn,
        plan_id,
        &wall,
        &format!("Progress check on {title}."),
        None,
        None,
        Some(session_id),
        now,
    )?;
    crate::log::record(
        conn,
        Some(user_id),
        "trigger_laid",
        &format!("event {event_id} at {} {wall}: progress check", at.date()),
    )
}

/// Closes the user's open session and drops the checks it had waiting; ending
/// nothing is success, so a client that lost track can always say stop.
/// `id` restricts the close to one session, for a client naming the one it
/// started.
pub fn end(
    conn: &Connection,
    user_id: i64,
    id: Option<i64>,
    outcome: &str,
    now: jiff::Timestamp,
) -> Result<Option<i64>> {
    anyhow::ensure!(outcome == "done" || outcome == "stopped", "invalid outcome: {outcome}");
    let Some(session) = open(conn, user_id)? else {
        return Ok(None);
    };
    if id.is_some_and(|wanted| wanted != session.id) {
        return Ok(None);
    }
    let tx = conn.is_autocommit().then(|| conn.unchecked_transaction()).transpose()?;
    conn.execute(
        "UPDATE work_sessions SET ended_at = ?1, outcome = ?2 WHERE id = ?3",
        (now.to_string(), outcome, session.id),
    )?;
    let dropped = conn.execute(
        "UPDATE events SET status = 'dropped', decided_at = ?1
         WHERE work_session_id = ?2 AND status IN ('pending','snoozed')",
        (now.to_string(), session.id),
    )?;
    if dropped > 0 {
        crate::log::record(
            conn,
            Some(user_id),
            "trigger_cancelled",
            &format!("{dropped} check{} left with work session {}",
                if dropped == 1 { "" } else { "s" }, session.id),
        )?;
    }
    crate::log::record(
        conn,
        Some(user_id),
        "work_session_ended",
        &format!("session {} {outcome}", session.id),
    )?;
    if let Some(tx) = tx {
        tx.commit()?;
    }
    Ok(Some(session.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> (Connection, tempfile::TempDir, i64) {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("defaults/user.toml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "display_name = \"X\"\ntimezone = \"UTC\"\ntemplate = \"default\"\n")
            .unwrap();
        (conn, tmp, uid)
    }

    fn at(ts: &str) -> jiff::Timestamp {
        ts.parse().unwrap()
    }

    fn start_one(
        conn: &Connection,
        tmp: &tempfile::TempDir,
        uid: i64,
        planned_min: Option<i64>,
    ) -> Session {
        start(
            conn,
            tmp.path(),
            uid,
            "aki",
            NewSession { title: "read the chapter".into(), planned_min, ..Default::default() },
            at("2026-09-17T09:00:00Z"),
        )
        .unwrap()
    }

    fn checks(conn: &Connection) -> Vec<(i64, String, String, String)> {
        let mut stmt = conn
            .prepare(
                "SELECT work_session_id, wall_time, status, prompt FROM events
                 WHERE kind = 'trigger' ORDER BY id",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn the_first_check_lands_halfway_in_or_at_twenty_five_minutes() {
        let (conn, tmp, uid) = env();
        start_one(&conn, &tmp, uid, Some(30));
        assert_eq!(checks(&conn)[0].1, "09:15");

        let (conn, tmp, uid) = env();
        start_one(&conn, &tmp, uid, Some(120));
        assert_eq!(checks(&conn)[0].1, "09:25");

        let (conn, tmp, uid) = env();
        start_one(&conn, &tmp, uid, None);
        let first = &checks(&conn)[0];
        assert_eq!((first.0, first.1.as_str()), (1, "09:25"));
        assert_eq!(first.3, "Progress check on read the chapter.");
    }

    #[test]
    fn a_new_session_stops_the_one_still_running() {
        let (conn, tmp, uid) = env();
        let first = start_one(&conn, &tmp, uid, Some(30));
        let second = start_one(&conn, &tmp, uid, Some(30));
        assert_ne!(first.id, second.id);
        assert_eq!(open(&conn, uid).unwrap().unwrap().id, second.id);
        let outcome: String = conn
            .query_row("SELECT outcome FROM work_sessions WHERE id = ?1", [first.id], |r| r.get(0))
            .unwrap();
        assert_eq!(outcome, "stopped");
        let rows = checks(&conn);
        assert_eq!(rows[0].2, "dropped", "the stopped session took its check with it");
        assert_eq!(rows[1].2, "pending");
    }

    #[test]
    fn ending_a_session_drops_its_waiting_checks_and_leaves_the_rest_alone() {
        let (conn, tmp, uid) = env();
        let session = start_one(&conn, &tmp, uid, Some(60));
        crate::triggers::lay(
            &conn,
            &crate::triggers::Lay {
                config_dir: tmp.path(),
                user_id: uid,
                username: "aki",
                at: "09:40",
                prompt: "still going?",
                date: "2026-09-17".parse().unwrap(),
                cancel: None,
                conversation_id: None,
                now: at("2026-09-17T09:00:00Z"),
            },
        )
        .unwrap();
        conn.execute("UPDATE events SET status = 'fired' WHERE wall_time = '09:25'", []).unwrap();

        assert_eq!(end(&conn, uid, Some(session.id), "done", at("2026-09-17T09:35:00Z")).unwrap(),
                   Some(session.id));
        let rows = checks(&conn);
        assert_eq!(rows[0].2, "fired", "a check already sent stays as it went");
        assert_eq!(rows[1].2, "dropped");
        assert!(open(&conn, uid).unwrap().is_none());
        assert_eq!(end(&conn, uid, None, "done", at("2026-09-17T09:36:00Z")).unwrap(), None);
    }

    #[test]
    fn ending_by_a_stale_id_leaves_the_running_session_alone() {
        let (conn, tmp, uid) = env();
        let session = start_one(&conn, &tmp, uid, Some(60));
        assert_eq!(end(&conn, uid, Some(session.id + 99), "done", at("2026-09-17T09:35:00Z")).unwrap(), None);
        assert_eq!(open(&conn, uid).unwrap().unwrap().id, session.id);
    }

    #[test]
    fn a_session_refuses_a_blank_title_a_silly_length_and_a_foreign_reference() {
        let (conn, tmp, uid) = env();
        let bad = |new: NewSession| {
            start(&conn, tmp.path(), uid, "aki", new, at("2026-09-17T09:00:00Z")).unwrap_err()
        };
        assert!(matches!(bad(NewSession { title: "  ".into(), ..Default::default() }), StartError::Invalid(_)));
        assert!(matches!(
            bad(NewSession { title: "x".into(), planned_min: Some(0), ..Default::default() }),
            StartError::Invalid(_)
        ));
        assert!(matches!(
            bad(NewSession { title: "x".into(), task_id: Some(404), ..Default::default() }),
            StartError::Invalid(_)
        ));
        assert!(matches!(
            bad(NewSession { title: "x".into(), event_id: Some(404), ..Default::default() }),
            StartError::Invalid(_)
        ));
        assert!(open(&conn, uid).unwrap().is_none());
        let events: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(events, 0);
    }
}
