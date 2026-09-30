use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

pub const PLAN_FACTOR: &str = "plan_factor";
/// The stretch of finished work one run reads, and the shortest run it counts:
/// a couple of minutes says nothing about how long the work takes.
const WINDOW_HOURS: i64 = 14 * 24;
const MIN_RUN_MIN: i64 = 5;
const MIN_SAMPLE: usize = 3;
const FLOOR: f64 = 0.5;
const CEILING: f64 = 2.0;
/// The grain a block is laid on.
const GRAIN: i64 = 5;

/// A learned value with the number of sessions standing behind it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Learned {
    pub value: f64,
    pub sample: i64,
}

/// How long the user's work runs against what was planned for it; absent until
/// enough sessions have ended to mean anything.
pub fn plan_factor(conn: &Connection, user_id: i64) -> rusqlite::Result<Option<Learned>> {
    conn.query_row(
        "SELECT value, sample FROM learning WHERE user_id = ?1 AND key = ?2",
        (user_id, PLAN_FACTOR),
        |r| Ok(Learned { value: r.get(0)?, sample: r.get(1)? }),
    )
    .optional()
}

/// A planned duration as the factor says it will really run, rounded up to the
/// grain. Without a factor the duration stands as it was.
pub fn stretch(minutes: i64, factor: Option<Learned>) -> i64 {
    let Some(f) = factor else { return minutes };
    let stretched = (minutes as f64 * f.value / GRAIN as f64).ceil() as i64 * GRAIN;
    stretched.clamp(GRAIN, i64::from(u16::MAX))
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len();
    match n % 2 {
        0 => f64::midpoint(values[n / 2 - 1], values[n / 2]),
        _ => values[n / 2],
    }
}

/// Elapsed over planned for every session in the window that finished what it
/// set out to do and ran long enough to be told from a false start.
fn ratios(conn: &Connection, user_id: i64, now: jiff::Timestamp) -> rusqlite::Result<Vec<f64>> {
    let since = now
        .checked_sub(jiff::Span::new().hours(WINDOW_HOURS))
        .unwrap_or(jiff::Timestamp::MIN);
    let mut stmt = conn.prepare(
        "SELECT planned_min, started_at, ended_at, paused_ms FROM work_sessions
         WHERE user_id = ?1 AND outcome = 'done' AND planned_min > 0 AND ended_at >= ?2",
    )?;
    let rows = stmt.query_map((user_id, since.to_string()), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (planned, started, ended, paused) = row?;
        let (Ok(started), Ok(ended)) =
            (started.parse::<jiff::Timestamp>(), ended.parse::<jiff::Timestamp>())
        else {
            continue;
        };
        let elapsed = (ended.as_millisecond() - started.as_millisecond() - paused).max(0) / 60_000;
        if elapsed < MIN_RUN_MIN {
            continue;
        }
        out.push(elapsed as f64 / planned as f64);
    }
    Ok(out)
}

/// Turns the sessions that ended into what the planner should assume; the
/// nightly runs it once the day's harvest is in.
pub fn run_for_user(conn: &Connection, user_id: i64, now: jiff::Timestamp) -> Result<()> {
    let mut ratios = ratios(conn, user_id, now)?;
    if ratios.len() < MIN_SAMPLE {
        conn.execute(
            "DELETE FROM learning WHERE user_id = ?1 AND key = ?2",
            (user_id, PLAN_FACTOR),
        )?;
        return Ok(());
    }
    let sample = ratios.len() as i64;
    let value = median(&mut ratios).clamp(FLOOR, CEILING);
    conn.execute(
        "INSERT INTO learning (user_id, key, value, sample, computed_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (user_id, key) DO UPDATE
             SET value = ?3, sample = ?4, computed_at = ?5",
        (user_id, PLAN_FACTOR, value, sample, now.to_string()),
    )?;
    crate::log::record(
        conn,
        Some(user_id),
        "learned",
        &format!("plan_factor {value:.2} from {sample} sessions"),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> (Connection, i64) {
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "pw", false).unwrap();
        (conn, uid)
    }

    fn at(when: &str) -> jiff::Timestamp {
        when.parse().unwrap()
    }

    /// A session of `planned` minutes that really ran `elapsed`, ending `days_ago`.
    fn session(conn: &Connection, uid: i64, planned: i64, elapsed: i64, days_ago: i64) {
        let ended = at("2026-09-18T20:00:00Z") - jiff::Span::new().hours(days_ago * 24);
        let started = ended - jiff::Span::new().minutes(elapsed);
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, planned_min, started_at, ended_at, outcome)
             VALUES (?1, 'the chapter', ?2, ?3, ?4, 'done')",
            (uid, planned, started.to_string(), ended.to_string()),
        )
        .unwrap();
    }

    fn now() -> jiff::Timestamp {
        at("2026-09-18T21:00:00Z")
    }

    #[test]
    fn the_factor_is_the_middle_session_not_the_average() {
        let (conn, uid) = env();
        session(&conn, uid, 30, 30, 1);
        session(&conn, uid, 30, 45, 2);
        session(&conn, uid, 30, 150, 3);
        run_for_user(&conn, uid, now()).unwrap();
        let learned = plan_factor(&conn, uid).unwrap().unwrap();
        assert_eq!(learned, Learned { value: 1.5, sample: 3 }, "the outlier moves the order, not the value");
    }

    #[test]
    fn an_even_sample_takes_the_middle_pair() {
        let (conn, uid) = env();
        session(&conn, uid, 30, 30, 1);
        session(&conn, uid, 30, 30, 2);
        session(&conn, uid, 30, 60, 3);
        session(&conn, uid, 30, 60, 4);
        run_for_user(&conn, uid, now()).unwrap();
        assert!((plan_factor(&conn, uid).unwrap().unwrap().value - 1.5).abs() < 1e-9);
    }

    #[test]
    fn the_factor_is_held_between_a_half_and_double() {
        let (conn, uid) = env();
        for d in 1..=3 {
            session(&conn, uid, 10, 120, d);
        }
        run_for_user(&conn, uid, now()).unwrap();
        assert!((plan_factor(&conn, uid).unwrap().unwrap().value - CEILING).abs() < 1e-9);

        let (conn, uid) = env();
        for d in 1..=3 {
            session(&conn, uid, 120, 6, d);
        }
        run_for_user(&conn, uid, now()).unwrap();
        assert!((plan_factor(&conn, uid).unwrap().unwrap().value - FLOOR).abs() < 1e-9);
    }

    #[test]
    fn two_sessions_teach_nothing() {
        let (conn, uid) = env();
        session(&conn, uid, 30, 45, 1);
        session(&conn, uid, 30, 45, 2);
        run_for_user(&conn, uid, now()).unwrap();
        assert!(plan_factor(&conn, uid).unwrap().is_none());
    }

    #[test]
    fn a_factor_goes_when_the_sessions_behind_it_age_out() {
        let (conn, uid) = env();
        for d in 1..=3 {
            session(&conn, uid, 30, 45, d);
        }
        run_for_user(&conn, uid, now()).unwrap();
        assert!(plan_factor(&conn, uid).unwrap().is_some());
        run_for_user(&conn, uid, now() + jiff::Span::new().hours(20 * 24)).unwrap();
        assert!(plan_factor(&conn, uid).unwrap().is_none());
    }

    #[test]
    fn short_runs_unfinished_work_and_unplanned_sessions_are_left_out() {
        let (conn, uid) = env();
        session(&conn, uid, 30, 45, 1);
        session(&conn, uid, 30, 45, 2);
        session(&conn, uid, 30, 4, 3);
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, planned_min, started_at, ended_at, outcome)
             VALUES (?1, 'stopped', 30, '2026-09-17T09:00:00Z', '2026-09-17T10:00:00Z', 'stopped')",
            [uid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO work_sessions (user_id, title, started_at, ended_at, outcome)
             VALUES (?1, 'no plan', '2026-09-17T09:00:00Z', '2026-09-17T10:00:00Z', 'done')",
            [uid],
        )
        .unwrap();
        run_for_user(&conn, uid, now()).unwrap();
        assert!(plan_factor(&conn, uid).unwrap().is_none(), "only two sessions count");
    }

    #[test]
    fn a_pause_does_not_count_as_work() {
        let (conn, uid) = env();
        for d in 1..=3 {
            session(&conn, uid, 30, 60, d);
        }
        conn.execute("UPDATE work_sessions SET paused_ms = 30 * 60000", []).unwrap();
        run_for_user(&conn, uid, now()).unwrap();
        assert!((plan_factor(&conn, uid).unwrap().unwrap().value - 1.0).abs() < 1e-9);
    }

    #[test]
    fn one_account_learns_nothing_from_another() {
        let (conn, uid) = env();
        let other = crate::auth::create_user(&conn, "bo", "pw", false).unwrap();
        for d in 1..=3 {
            session(&conn, uid, 30, 60, d);
        }
        run_for_user(&conn, other, now()).unwrap();
        assert!(plan_factor(&conn, other).unwrap().is_none());
    }

    #[test]
    fn a_stretched_duration_lands_on_the_grain() {
        let factor = Some(Learned { value: 1.4, sample: 9 });
        assert_eq!(stretch(25, factor), 35);
        assert_eq!(stretch(30, factor), 45, "42 minutes rounds up to the grain");
        assert_eq!(stretch(30, None), 30);
        assert_eq!(stretch(5, Some(Learned { value: 0.5, sample: 3 })), 5);
    }
}
