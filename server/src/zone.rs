use anyhow::Result;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::Timestamp;
use rusqlite::Connection;
use std::path::Path;

/// The instant a zero-padded `HH:MM` on `date` names in `tz`, resolved the way
/// the runner fires it (a DST gap resolves forward); `None` when `wall` is
/// malformed.
pub fn instant(tz: &TimeZone, date: Date, wall: &str) -> Option<Timestamp> {
    if !crate::templates::valid_time(wall) {
        return None;
    }
    let time: jiff::civil::Time = format!("{wall}:00").parse().ok()?;
    tz.to_ambiguous_zoned(date.to_datetime(time)).compatible().ok().map(|z| z.timestamp())
}

/// The plan date and `HH:MM` an instant reads as in `tz`.
pub fn civil(at: Timestamp, tz: &TimeZone) -> (Date, String) {
    let z = at.to_zoned(tz.clone());
    (z.date(), format!("{:02}:{:02}", z.hour(), z.minute()))
}

/// A wall time on `date` as read in `from`, restated in `to` so it names the
/// same instant.
pub fn translate(date: Date, wall: &str, from: &TimeZone, to: &TimeZone) -> Option<(Date, String)> {
    instant(from, date, wall).map(|at| civil(at, to))
}

/// A zone name as the runner reads it: unknown names fall back to UTC.
pub fn named(name: &str) -> TimeZone {
    TimeZone::get(name).unwrap_or(TimeZone::UTC)
}

struct Pending {
    id: i64,
    date: Date,
    wall: String,
    orig: String,
    end: Option<String>,
}

/// Restates every still-pending event of the user that is not template
/// furniture so it fires at the same instant under `to` as it would have under
/// `from`, moving it to the plan of the date it now falls on. Template events
/// keep their civil time, and anything already due is left for the runner. A
/// block the move would carry past midnight ends at 23:59. Whole-day dues stay
/// on their day and end where it ends under `to`. Returns how many events
/// moved.
pub fn rezone(
    conn: &Connection,
    config_dir: &Path,
    username: &str,
    user_id: i64,
    from: &str,
    to: &str,
    now: Timestamp,
) -> Result<usize> {
    let (old, new) = (named(from), named(to));
    let pending: Vec<Pending> = {
        let mut stmt = conn.prepare(
            "SELECT e.id, p.date, e.wall_time, e.orig_wall_time, e.end_wall_time
             FROM events e JOIN plans p ON p.id = e.plan_id
             WHERE p.user_id = ?1 AND e.status IN ('pending', 'snoozed') AND e.origin != 'template'",
        )?;
        let rows = stmt.query_map([user_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, date, wall, orig, end) = row?;
            if let Ok(date) = date.parse() {
                out.push(Pending { id, date, wall, orig, end });
            }
        }
        out
    };
    let mut moved = 0;
    for ev in pending {
        let Some(start) = instant(&old, ev.date, &ev.wall).filter(|at| *at > now) else {
            continue;
        };
        let (date, wall) = civil(start, &new);
        let orig = translate(ev.date, &ev.orig, &old, &new).map_or(ev.orig, |(_, w)| w);
        let end = ev.end.map(|end| {
            let Some(mut at) = instant(&old, ev.date, &end) else { return end };
            if at <= start {
                at += jiff::SignedDuration::from_hours(24);
            }
            let (end_date, end_wall) = civil(at, &new);
            if end_date > date { "23:59".to_string() } else { end_wall }
        });
        let plan_id = crate::plan::holding(conn, config_dir, username, user_id, date, now.to_zoned(new.clone()).date())?;
        conn.execute(
            "UPDATE events SET plan_id = ?1, wall_time = ?2, orig_wall_time = ?3, end_wall_time = ?4
             WHERE id = ?5",
            (plan_id, &wall, &orig, &end, ev.id),
        )?;
        moved += 1;
    }
    crate::due::rezone(conn, user_id, &new)?;
    crate::log::record(
        conn,
        Some(user_id),
        "timezone_changed",
        &format!("{from} → {to}: {moved} pending events kept their instant"),
    )?;
    Ok(moved)
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
        std::fs::write(p, "display_name = \"X\"\ntimezone = \"America/Los_Angeles\"\ntemplate = \"default\"\n")
            .unwrap();
        (conn, tmp, uid)
    }

    fn at(ts: &str) -> Timestamp {
        ts.parse().unwrap()
    }

    fn event(conn: &Connection, tmp: &tempfile::TempDir, uid: i64, date: &str, wall: &str, origin: &str) -> i64 {
        let plan = crate::plan::ensure(conn, tmp.path(), "aitest", uid, date.parse().unwrap()).unwrap();
        conn.execute(
            "INSERT INTO events (plan_id, kind, wall_time, orig_wall_time, origin) VALUES (?1, 'trigger', ?2, ?2, ?3)",
            (plan, wall, origin),
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn row(conn: &Connection, id: i64) -> (String, String, String, Option<String>) {
        conn.query_row(
            "SELECT p.date, e.wall_time, e.orig_wall_time, e.end_wall_time
             FROM events e JOIN plans p ON p.id = e.plan_id WHERE e.id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
    }

    fn rezoned(conn: &Connection, tmp: &tempfile::TempDir, uid: i64, now: &str) -> usize {
        rezone(conn, tmp.path(), "aitest", uid, "America/Los_Angeles", "America/New_York", at(now)).unwrap()
    }

    /// 2026-10-11 09:00 PDT is 16:00Z; the agent's 11:00 PDT check is 14:00 EDT.
    #[test]
    fn a_pending_check_keeps_its_instant_and_a_template_one_its_clock() {
        let (conn, tmp, uid) = env();
        let agent = event(&conn, &tmp, uid, "2026-10-11", "11:00", "agent");
        let template = event(&conn, &tmp, uid, "2026-10-11", "21:30", "template");
        assert_eq!(rezoned(&conn, &tmp, uid, "2026-10-11T16:00:00Z"), 1);
        assert_eq!(row(&conn, agent), ("2026-10-11".into(), "14:00".into(), "14:00".into(), None));
        assert_eq!(row(&conn, template).1, "21:30");
        let logged: String = conn
            .query_row("SELECT detail FROM event_log WHERE kind = 'timezone_changed'", [], |r| r.get(0))
            .unwrap();
        assert!(logged.contains("America/New_York"), "{logged}");
    }

    #[test]
    fn a_check_pushed_past_midnight_moves_to_the_next_days_plan() {
        let (conn, tmp, uid) = env();
        let id = event(&conn, &tmp, uid, "2026-10-11", "22:30", "user");
        rezoned(&conn, &tmp, uid, "2026-10-11T16:00:00Z");
        assert_eq!(row(&conn, id).0, "2026-10-12");
        assert_eq!(row(&conn, id).1, "01:30");
        assert!(!crate::plan::exists(&conn, uid, "2026-10-12".parse().unwrap()).unwrap(), "a later day waits bare");
    }

    #[test]
    fn what_is_due_or_decided_is_left_alone() {
        let (conn, tmp, uid) = env();
        let gone = event(&conn, &tmp, uid, "2026-10-11", "08:00", "agent");
        let done = event(&conn, &tmp, uid, "2026-10-11", "12:00", "idle");
        conn.execute("UPDATE events SET status = 'done' WHERE id = ?1", [done]).unwrap();
        let snoozed = event(&conn, &tmp, uid, "2026-10-11", "12:00", "lay_day");
        conn.execute("UPDATE events SET status = 'snoozed', wall_time = '12:15' WHERE id = ?1", [snoozed])
            .unwrap();
        assert_eq!(rezoned(&conn, &tmp, uid, "2026-10-11T16:00:00Z"), 1);
        assert_eq!(row(&conn, gone).1, "08:00");
        assert_eq!(row(&conn, done).1, "12:00");
        assert_eq!(row(&conn, snoozed), ("2026-10-11".into(), "15:15".into(), "15:00".into(), None));
    }

    #[test]
    fn a_block_keeps_its_length_until_midnight_stops_it() {
        let (conn, tmp, uid) = env();
        let block = event(&conn, &tmp, uid, "2026-10-11", "13:00", "auto");
        conn.execute("UPDATE events SET alert = 0, end_wall_time = '14:30' WHERE id = ?1", [block]).unwrap();
        let late = event(&conn, &tmp, uid, "2026-10-11", "20:00", "auto");
        conn.execute("UPDATE events SET alert = 0, end_wall_time = '22:00' WHERE id = ?1", [late]).unwrap();
        rezoned(&conn, &tmp, uid, "2026-10-11T16:00:00Z");
        assert_eq!(row(&conn, block).3.as_deref(), Some("17:30"));
        assert_eq!((row(&conn, late).1, row(&conn, late).3), ("23:00".into(), Some("23:59".into())));
    }

    #[test]
    fn translate_restates_a_wall_time_across_the_date_line() {
        let tokyo = TimeZone::get("Asia/Tokyo").unwrap();
        let la = TimeZone::get("America/Los_Angeles").unwrap();
        let (date, wall) = translate("2026-10-12".parse().unwrap(), "11:00", &tokyo, &la).unwrap();
        assert_eq!((date.to_string(), wall.as_str()), ("2026-10-11".into(), "19:00"));
        assert!(translate("2026-10-12".parse().unwrap(), "9:00", &tokyo, &la).is_none());
    }

    #[test]
    fn a_whole_day_stays_on_its_day_and_a_moment_keeps_its_instant() {
        let (conn, tmp, uid) = env();
        let la = named("America/Los_Angeles");
        let new = |due: &str, day: bool| {
            let t = crate::tasks::create(
                &conn,
                uid,
                crate::tasks::NewTask {
                    title: due.into(),
                    due_at: (!day).then(|| Some(due.to_owned())),
                    due_on: day.then(|| Some(due.to_owned())),
                    zone: Some(la.clone()),
                    ..Default::default()
                },
                "manual",
                crate::tasks::Actor::User,
            )
            .unwrap();
            t.id
        };
        let day = new("2026-10-20", true);
        let moment = new("2026-10-20T16:00:00Z", false);
        rezoned(&conn, &tmp, uid, "2026-10-11T16:00:00Z");
        let get = |id| crate::tasks::get(&conn, uid, id).unwrap().unwrap();
        assert_eq!(get(day).due_at.as_deref(), Some("2026-10-21T03:59:00Z"), "23:59 EDT");
        assert_eq!(get(day).due_on.as_deref(), Some("2026-10-20"));
        assert_eq!(get(moment).due_at.as_deref(), Some("2026-10-20T16:00:00Z"));
    }
}
