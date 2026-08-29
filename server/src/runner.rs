use crate::{config::UserConfig, AppState};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::Path;

struct Candidate {
    event_id: i64,
    user_id: i64,
    username: String,
    date: String,
    wall_time: String,
}

/// Resolves the candidate's stored wall time to an instant in `tz`. `date` and
/// `wall_time` come from user-authored templates and are stored unvalidated, so
/// both parses can fail. DST gaps resolve forward to the next valid instant.
fn due_at(tz: &jiff::tz::TimeZone, c: &Candidate) -> Result<jiff::Timestamp> {
    let date: jiff::civil::Date = c.date.parse()?;
    let time: jiff::civil::Time = format!("{}:00", c.wall_time).parse()?;
    Ok(tz.to_ambiguous_zoned(date.to_datetime(time)).compatible()?.timestamp())
}

/// Resolves a user's configured timezone, memoized so a sweep reads each user's
/// config — and reports a bad timezone — only once. A missing config or an
/// unknown timezone name degrades to UTC; the unknown name is recorded as a
/// `runner_error` rather than silently changing when the user's events fire.
fn user_tz(
    conn: &Connection,
    config_dir: &Path,
    c: &Candidate,
    cache: &mut HashMap<String, jiff::tz::TimeZone>,
) -> Result<jiff::tz::TimeZone> {
    if let Some(tz) = cache.get(&c.username) {
        return Ok(tz.clone());
    }
    let name = UserConfig::load(config_dir, &c.username)
        .map(|u| u.timezone)
        .unwrap_or_else(|_| "UTC".into());
    let tz = match jiff::tz::TimeZone::get(&name) {
        Ok(tz) => tz,
        Err(_) => {
            crate::log::record(
                conn,
                Some(c.user_id),
                "runner_error",
                &format!("user {} has unknown timezone {name:?}; using UTC", c.username),
            )?;
            jiff::tz::TimeZone::UTC
        }
    };
    cache.insert(c.username.clone(), tz.clone());
    Ok(tz)
}

/// Fires every pending or snoozed event whose wall time, resolved in its user's
/// timezone on the plan date, has arrived by `now`. A candidate that cannot be
/// resolved is logged as `runner_error` and skipped, so one unusable row cannot
/// stall the sweep for every other user.
pub fn fire_due(conn: &Connection, config_dir: &Path, now: jiff::Timestamp) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT e.id, p.user_id, u.username, p.date, e.wall_time
         FROM events e
         JOIN plans p ON p.id = e.plan_id
         JOIN users u ON u.id = p.user_id
         WHERE e.status IN ('pending','snoozed')",
    )?;
    let candidates: Vec<Candidate> = stmt
        .query_map([], |r| {
            Ok(Candidate {
                event_id: r.get(0)?,
                user_id: r.get(1)?,
                username: r.get(2)?,
                date: r.get(3)?,
                wall_time: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut fired = Vec::new();
    let mut tz_cache = HashMap::new();
    for c in candidates {
        let tz = user_tz(conn, config_dir, &c, &mut tz_cache)?;
        let due = match due_at(&tz, &c) {
            Ok(due) => due,
            Err(e) => {
                crate::log::record(
                    conn,
                    Some(c.user_id),
                    "runner_error",
                    &format!(
                        "event {} unresolvable ({} {}): {e}",
                        c.event_id, c.date, c.wall_time
                    ),
                )?;
                continue;
            }
        };
        if due <= now {
            conn.execute(
                "UPDATE events SET status='fired', fired_at=?1 WHERE id=?2",
                (now.to_string(), c.event_id),
            )?;
            crate::log::record(
                conn,
                Some(c.user_id),
                "event_fired",
                &format!("event {} due {}", c.event_id, due),
            )?;
            fired.push(c.event_id);
        }
    }
    Ok(fired)
}

pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            tick.tick().await;
            // The guard is confined to this iteration: holding it across the
            // next `tick` would make the task non-Send and stall handlers.
            let conn = state.db.lock().unwrap();
            if let Err(e) = fire_due(&conn, &state.config_dir, jiff::Timestamp::now()) {
                let _ = crate::log::record(&conn, None, "runner_error", &e.to_string());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::{Template, TemplateEvent};

    fn setup(tz: &str) -> (rusqlite::Connection, tempfile::TempDir, i64) {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("users/aki");
        std::fs::create_dir_all(tmp.path().join("defaults")).unwrap();
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            tmp.path().join("defaults/user.toml"),
            format!("display_name = \"X\"\ntimezone = \"{tz}\"\ntemplate = \"default\"\n"),
        ).unwrap();
        let conn = crate::db::open_memory().unwrap();
        let uid = crate::auth::create_user(&conn, "aki", "p", false).unwrap();
        (conn, tmp, uid)
    }

    fn one_event_template(time: &str) -> Template {
        Template { events: vec![TemplateEvent {
            kind: "nudge".into(), time: time.into(),
            days: vec!["mon".into(),"tue".into(),"wed".into(),"thu".into(),"fri".into(),"sat".into(),"sun".into()],
            flexibility: "slide".into(), slide_window_min: 60, channel: "push".into(),
        }]}
    }

    #[test]
    fn fires_only_when_due_in_user_tz() {
        let (conn, tmp, uid) = setup("Asia/Tokyo");
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(&conn, uid, &one_event_template("09:00"), date).unwrap();

        // 08:59 JST on the plan date = 2026-08-30T23:59Z
        let early: jiff::Timestamp = "2026-08-30T23:59:00Z".parse().unwrap();
        assert!(fire_due(&conn, tmp.path(), early).unwrap().is_empty());

        // 09:01 JST
        let due: jiff::Timestamp = "2026-08-31T00:01:00Z".parse().unwrap();
        let fired = fire_due(&conn, tmp.path(), due).unwrap();
        assert_eq!(fired.len(), 1);

        // second run does not double-fire
        assert!(fire_due(&conn, tmp.path(), due).unwrap().is_empty());
    }

    #[test]
    fn unresolvable_event_is_logged_and_skipped() {
        let (conn, tmp, uid) = setup("UTC");
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        let plan_id = crate::plan::generate(&conn, uid, &one_event_template("00:00"), date).unwrap();
        let good_id = crate::plan::events_for(&conn, uid, date).unwrap()[0].id;
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time) VALUES (?1, 'nudge', 'noon')",
            [plan_id],
        ).unwrap();

        let now: jiff::Timestamp = "2026-08-31T12:00:00Z".parse().unwrap();
        assert_eq!(fire_due(&conn, tmp.path(), now).unwrap(), vec![good_id]);

        let errors: i64 = conn.query_row(
            "SELECT COUNT(*) FROM event_log WHERE kind='runner_error'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(errors, 1);
        let still_pending: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE wall_time='noon' AND status='pending'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(still_pending, 1);
    }

    #[test]
    fn unknown_timezone_falls_back_to_utc_and_is_logged() {
        let (conn, tmp, uid) = setup("Asia/Toyko");
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(&conn, uid, &one_event_template("09:00"), date).unwrap();

        let now: jiff::Timestamp = "2026-08-31T09:30:00Z".parse().unwrap();
        assert_eq!(fire_due(&conn, tmp.path(), now).unwrap().len(), 1);

        let detail: String = conn.query_row(
            "SELECT detail FROM event_log WHERE kind='runner_error'", [], |r| r.get(0),
        ).unwrap();
        assert!(detail.contains("Asia/Toyko"), "unexpected detail: {detail}");
    }

    #[test]
    fn snoozed_events_fire_when_due() {
        let (conn, tmp, uid) = setup("UTC");
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(&conn, uid, &one_event_template("09:00"), date).unwrap();
        conn.execute("UPDATE events SET status='snoozed', wall_time='09:30'", []).unwrap();
        let now: jiff::Timestamp = "2026-08-31T09:31:00Z".parse().unwrap();
        assert_eq!(fire_due(&conn, tmp.path(), now).unwrap().len(), 1);
    }

    #[test]
    fn fired_events_are_logged() {
        let (conn, tmp, uid) = setup("UTC");
        let date: jiff::civil::Date = "2026-08-31".parse().unwrap();
        crate::plan::generate(&conn, uid, &one_event_template("00:00"), date).unwrap();
        let now: jiff::Timestamp = "2026-08-31T12:00:00Z".parse().unwrap();
        fire_due(&conn, tmp.path(), now).unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM event_log WHERE kind='event_fired'", [], |r| r.get(0),
        ).unwrap();
        assert_eq!(n, 1);
    }
}
