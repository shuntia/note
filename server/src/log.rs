use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

pub const ERROR_LOG_WINDOW_MINS: i64 = 60;

pub fn record(conn: &Connection, user_id: Option<i64>, kind: &str, detail: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO event_log (ts, user_id, kind, detail) VALUES (?1, ?2, ?3, ?4)",
        (jiff::Timestamp::now().to_string(), user_id, kind, detail),
    )?;
    Ok(())
}

/// Writes the row unless an identical (user, kind, detail) row younger than
/// `window_mins` already exists — recurring failures (a broken user config
/// hit every sweep) log once per window instead of once per tick. Returns
/// whether a row was written.
pub fn record_throttled(
    conn: &Connection,
    user_id: Option<i64>,
    kind: &str,
    detail: &str,
    now: jiff::Timestamp,
    window_mins: i64,
) -> Result<bool> {
    let last: Option<String> = conn
        .query_row(
            "SELECT ts FROM event_log
             WHERE user_id IS ?1 AND kind = ?2 AND detail = ?3
             ORDER BY id DESC LIMIT 1",
            (user_id, kind, detail),
            |r| r.get(0),
        )
        .optional()?;
    if let Some(ts) = last.and_then(|s| s.parse::<jiff::Timestamp>().ok()) {
        // A negative span (stored ts ahead of `now` after a clock step) must
        // write, not suppress until the clock catches up.
        if (now - ts)
            .total(jiff::Unit::Second)
            .is_ok_and(|s| (0.0..(window_mins * 60) as f64).contains(&s))
        {
            return Ok(false);
        }
    }
    conn.execute(
        "INSERT INTO event_log (ts, user_id, kind, detail) VALUES (?1, ?2, ?3, ?4)",
        (now.to_string(), user_id, kind, detail),
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM event_log", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn identical_errors_inside_the_window_collapse_to_one_row() {
        let conn = crate::db::open_memory().unwrap();
        let t0: jiff::Timestamp = "2026-08-31T00:00:00Z".parse().unwrap();
        let t1: jiff::Timestamp = "2026-08-31T00:30:00Z".parse().unwrap();
        assert!(record_throttled(&conn, None, "runner_error", "boom", t0, 60).unwrap());
        assert!(!record_throttled(&conn, None, "runner_error", "boom", t1, 60).unwrap());
        assert_eq!(rows(&conn), 1);
    }

    #[test]
    fn window_expiry_different_detail_and_different_user_all_write() {
        let conn = crate::db::open_memory().unwrap();
        let t0: jiff::Timestamp = "2026-08-31T00:00:00Z".parse().unwrap();
        let t2: jiff::Timestamp = "2026-08-31T01:00:01Z".parse().unwrap();
        assert!(record_throttled(&conn, None, "runner_error", "boom", t0, 60).unwrap());
        assert!(record_throttled(&conn, None, "runner_error", "other", t0, 60).unwrap());
        assert!(record_throttled(&conn, Some(1), "runner_error", "boom", t0, 60).unwrap());
        assert!(record_throttled(&conn, None, "runner_error", "boom", t2, 60).unwrap());
        assert_eq!(rows(&conn), 4);
    }

    #[test]
    fn a_stored_row_ahead_of_now_does_not_suppress() {
        let conn = crate::db::open_memory().unwrap();
        let ahead: jiff::Timestamp = "2026-08-31T02:00:00Z".parse().unwrap();
        let t0: jiff::Timestamp = "2026-08-31T00:00:00Z".parse().unwrap();
        assert!(record_throttled(&conn, None, "runner_error", "boom", ahead, 60).unwrap());
        assert!(record_throttled(&conn, None, "runner_error", "boom", t0, 60).unwrap());
        assert_eq!(rows(&conn), 2);
    }
}
